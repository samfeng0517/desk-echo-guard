//! A small pure-Rust ONNX interpreter: float32 inference for the operator
//! set openWakeWord's models use (melspectrogram front-end, the embedding
//! CNN, and the DNN wake-word heads exported by openWakeWord's trainer).
//! Unsupported operators are rejected when the model is loaded.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, bail, ensure, Context, Result};

use crate::gemm::{self, PackedB, Scratch};
use crate::onnx::{self, Attr, Data, Node, Tensor};

#[derive(Clone, Copy, Debug)]
enum Un {
    Relu,
    Sigmoid,
    Tanh,
    Exp,
    Log,
    Sqrt,
    Neg,
    Abs,
    Reciprocal,
    Floor,
    Ceil,
    LeakyRelu(f32),
}

#[derive(Clone, Copy, Debug)]
enum Bin {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Max,
    Min,
}

#[derive(Clone, Copy, Debug)]
enum Red {
    Max,
    Min,
    Mean,
    Sum,
}

#[derive(Clone, Copy, Debug)]
enum AutoPad {
    NotSet,
    Valid,
    SameUpper,
    SameLower,
}

/// Spatial parameters shared by Conv and MaxPool (2-D; 1-D is H = 1).
#[derive(Clone, Debug)]
struct Window {
    one_d: bool,
    kernel: [usize; 2],
    strides: [usize; 2],
    dilations: [usize; 2],
    /// [top, left, bottom, right]
    pads: [usize; 4],
    auto_pad: AutoPad,
    ceil_mode: bool,
}

impl Window {
    fn from_node(n: &Node, kernel: Option<Vec<i64>>) -> Result<Self> {
        let kernel = kernel
            .or_else(|| n.ints("kernel_shape"))
            .ok_or_else(|| anyhow!("{} 缺少 kernel_shape", n.op))?;
        let rank = kernel.len();
        ensure!(rank == 1 || rank == 2, "{} 只支援 1-D / 2-D", n.op);
        let one_d = rank == 1;
        let two = |v: Option<Vec<i64>>, def: i64| -> Result<[usize; 2]> {
            let v = v.unwrap_or_else(|| vec![def; rank]);
            ensure!(v.len() == rank && v.iter().all(|&x| x >= 0), "{} 屬性錯誤", n.op);
            Ok(if one_d {
                [1, v[0] as usize]
            } else {
                [v[0] as usize, v[1] as usize]
            })
        };
        let pads = n.ints("pads").unwrap_or_else(|| vec![0; 2 * rank]);
        ensure!(
            pads.len() == 2 * rank && pads.iter().all(|&p| p >= 0),
            "{} pads 錯誤",
            n.op
        );
        let pads = if one_d {
            [0, pads[0] as usize, 0, pads[1] as usize]
        } else {
            [
                pads[0] as usize,
                pads[1] as usize,
                pads[2] as usize,
                pads[3] as usize,
            ]
        };
        let auto_pad = match n.string("auto_pad").as_deref() {
            None | Some("NOTSET") | Some("") => AutoPad::NotSet,
            Some("VALID") => AutoPad::Valid,
            Some("SAME_UPPER") => AutoPad::SameUpper,
            Some("SAME_LOWER") => AutoPad::SameLower,
            Some(p) => bail!("不支援的 auto_pad {p}"),
        };
        let w = Self {
            one_d,
            kernel: two(Some(kernel), 1)?,
            strides: two(n.ints("strides"), 1)?,
            dilations: two(n.ints("dilations"), 1)?,
            pads,
            auto_pad,
            ceil_mode: n.int("ceil_mode", 0) != 0,
        };
        ensure!(
            w.strides.iter().chain(&w.dilations).all(|&v| v > 0),
            "{} strides / dilations 錯誤",
            n.op
        );
        Ok(w)
    }

    /// (N, C, H, W) of an input of rank 3 (1-D) or 4 (2-D).
    fn nchw(&self, s: &[usize]) -> Result<[usize; 4]> {
        match (self.one_d, s) {
            (true, &[n, c, w]) => Ok([n, c, 1, w]),
            (false, &[n, c, h, w]) => Ok([n, c, h, w]),
            _ => bail!("輸入形狀 {s:?} 與 kernel 維度不符"),
        }
    }

    /// Output size and leading pad along axis `ax` (0 = H, 1 = W).
    fn out_dim(&self, ax: usize, len: usize) -> Result<(usize, usize)> {
        let (k, s, d) = (self.kernel[ax], self.strides[ax], self.dilations[ax]);
        let span = (k - 1) * d + 1;
        let (pb, pe) = match self.auto_pad {
            AutoPad::NotSet => (self.pads[ax], self.pads[ax + 2]),
            AutoPad::Valid => (0, 0),
            AutoPad::SameUpper | AutoPad::SameLower => {
                let out = len.div_ceil(s);
                let total = ((out - 1) * s + span).saturating_sub(len);
                let small = total / 2;
                match self.auto_pad {
                    AutoPad::SameUpper => (small, total - small),
                    _ => (total - small, small),
                }
            }
        };
        let full = len + pb + pe;
        ensure!(full >= span, "輸入長度 {len} 小於 kernel 範圍 {span}");
        let out = if self.ceil_mode {
            (full - span).div_ceil(s) + 1
        } else {
            (full - span) / s + 1
        };
        Ok((out, pb))
    }
}

#[derive(Debug)]
enum Op {
    Identity,
    Unary(Un),
    Binary(Bin),
    Clip {
        min: Option<f32>,
        max: Option<f32>,
    },
    Cast(i64),
    Unsqueeze(Option<Vec<i64>>),
    Squeeze(Option<Vec<i64>>),
    Reshape {
        allowzero: bool,
    },
    Flatten(i64),
    Transpose(Option<Vec<i64>>),
    Reduce {
        kind: Red,
        axes: Option<Vec<i64>>,
        keepdims: bool,
        noop_empty: bool,
    },
    Conv {
        win: Window,
        oc: usize,
        ic: usize,
        /// rows ordered (ky, kx, ci), columns = output channels
        w: PackedB,
        bias: Vec<f32>,
    },
    MaxPool(Window),
    MatMul(Option<PackedB>),
    Gemm {
        trans_a: bool,
        trans_b: bool,
        alpha: f32,
        beta: f32,
        b: Option<PackedB>,
    },
    LayerNorm {
        axis: i64,
        eps: f32,
    },
    Softmax(i64),
}

#[derive(Debug)]
struct Step {
    op: Op,
    name: String,
    /// value slots; `None` = optional input left out
    ins: Vec<Option<usize>>,
    out: usize,
    /// slots whose last use is this step (freed afterwards)
    free: Vec<usize>,
    /// elementwise op whose first input dies here: may reuse its buffer
    inplace: bool,
}

/// A loaded ONNX model with one input and (at least) one output.
#[derive(Debug)]
pub struct Model {
    steps: Vec<Step>,
    consts: Vec<(usize, Arc<Tensor>)>,
    n_slots: usize,
    input: usize,
    output: usize,
    input_dims: Vec<i64>,
    output_dims: Vec<i64>,
    values: Vec<Option<Arc<Tensor>>>,
    scratch: Scratch,
}

fn tensor_attr(n: &Node, k: &str) -> Option<Tensor> {
    match n.attrs.get(k) {
        Some(Attr::Tensor(t)) => Some(t.clone()),
        Some(Attr::Float(f)) => Some(Tensor::f32(vec![], vec![*f])),
        Some(Attr::Floats(v)) => Some(Tensor::f32(vec![v.len()], v.clone())),
        Some(Attr::Int(i)) => Some(Tensor::i64(vec![], vec![*i])),
        Some(Attr::Ints(v)) => Some(Tensor::i64(vec![v.len()], v.clone())),
        _ => None,
    }
}

impl Model {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        Self::from_bytes(&bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let g = onnx::parse_model(bytes)?;
        ensure!(g.inputs.len() == 1, "模型必須只有一個輸入（有 {}）", g.inputs.len());
        ensure!(!g.outputs.is_empty(), "模型沒有輸出");
        let mut slot: HashMap<String, usize> = HashMap::new();
        let mut consts = Vec::new();
        let mut known: HashMap<usize, Arc<Tensor>> = HashMap::new();
        let get_slot = |name: &str, slot: &mut HashMap<String, usize>| -> usize {
            let n = slot.len();
            *slot.entry(name.to_string()).or_insert(n)
        };
        let input = get_slot(&g.inputs[0].name, &mut slot);
        for (name, t) in g.initializers {
            let s = get_slot(&name, &mut slot);
            let t = Arc::new(t);
            known.insert(s, t.clone());
            consts.push((s, t));
        }
        let mut steps: Vec<Step> = Vec::new();
        for n in &g.nodes {
            ensure!(
                n.domain.is_empty() || n.domain == "ai.onnx",
                "不支援的 ONNX 運算子 {}::{}",
                n.domain,
                n.op
            );
            let mut ins = Vec::new();
            for i in &n.inputs {
                if i.is_empty() {
                    ins.push(None);
                } else {
                    let s = *slot.get(i).ok_or_else(|| {
                        anyhow!("節點 {}（{}）的輸入 {i} 未定義（圖未排序？）", n.name, n.op)
                    })?;
                    ins.push(Some(s));
                }
            }
            let out_name = n
                .outputs
                .first()
                .ok_or_else(|| anyhow!("節點 {} 沒有輸出", n.name))?;
            let konst = |i: usize| -> Option<Arc<Tensor>> {
                ins.get(i).copied().flatten().and_then(|s| known.get(&s).cloned())
            };
            let op = match n.op.as_str() {
                "Constant" => {
                    let t = ["value", "value_float", "value_floats", "value_int", "value_ints"]
                        .iter()
                        .find_map(|k| tensor_attr(n, k))
                        .ok_or_else(|| anyhow!("不支援的 Constant 節點 {}", n.name))?;
                    let s = get_slot(out_name, &mut slot);
                    let t = Arc::new(t);
                    known.insert(s, t.clone());
                    consts.push((s, t));
                    continue;
                }
                "Identity" | "Dropout" => Op::Identity,
                "Relu" => Op::Unary(Un::Relu),
                "Sigmoid" => Op::Unary(Un::Sigmoid),
                "Tanh" => Op::Unary(Un::Tanh),
                "Exp" => Op::Unary(Un::Exp),
                "Log" => Op::Unary(Un::Log),
                "Sqrt" => Op::Unary(Un::Sqrt),
                "Neg" => Op::Unary(Un::Neg),
                "Abs" => Op::Unary(Un::Abs),
                "Reciprocal" => Op::Unary(Un::Reciprocal),
                "Floor" => Op::Unary(Un::Floor),
                "Ceil" => Op::Unary(Un::Ceil),
                "LeakyRelu" => Op::Unary(Un::LeakyRelu(n.float("alpha", 0.01))),
                "Add" | "Sum" => Op::Binary(Bin::Add),
                "Sub" => Op::Binary(Bin::Sub),
                "Mul" => Op::Binary(Bin::Mul),
                "Div" => Op::Binary(Bin::Div),
                "Pow" => Op::Binary(Bin::Pow),
                "Max" => Op::Binary(Bin::Max),
                "Min" => Op::Binary(Bin::Min),
                "Clip" => Op::Clip {
                    min: n.attrs.get("min").map(|_| n.float("min", f32::MIN)),
                    max: n.attrs.get("max").map(|_| n.float("max", f32::MAX)),
                },
                "Cast" => {
                    let to = n.int("to", 0);
                    ensure!(to == 1 || to == 7, "Cast 只支援 float / int64（to = {to}）");
                    Op::Cast(to)
                }
                "Unsqueeze" => Op::Unsqueeze(n.ints("axes")),
                "Squeeze" => Op::Squeeze(n.ints("axes")),
                "Reshape" => Op::Reshape {
                    allowzero: n.int("allowzero", 0) != 0,
                },
                "Flatten" => Op::Flatten(n.int("axis", 1)),
                "Transpose" => Op::Transpose(n.ints("perm")),
                "ReduceMax" | "ReduceMin" | "ReduceMean" | "ReduceSum" => Op::Reduce {
                    kind: match n.op.as_str() {
                        "ReduceMax" => Red::Max,
                        "ReduceMin" => Red::Min,
                        "ReduceMean" => Red::Mean,
                        _ => Red::Sum,
                    },
                    axes: n.ints("axes"),
                    keepdims: n.int("keepdims", 1) != 0,
                    noop_empty: n.int("noop_with_empty_axes", 0) != 0,
                },
                "Conv" => {
                    ensure!(n.int("group", 1) == 1, "Conv 只支援 group = 1（{}）", n.name);
                    let w = konst(1)
                        .ok_or_else(|| anyhow!("Conv {} 的權重必須是常數", n.name))?;
                    let ws = &w.shape;
                    ensure!(ws.len() == 3 || ws.len() == 4, "Conv 權重形狀 {ws:?}");
                    let kshape: Vec<i64> = ws[2..].iter().map(|&v| v as i64).collect();
                    let win = Window::from_node(n, Some(n.ints("kernel_shape").unwrap_or(kshape)))?;
                    let (oc, ic) = (ws[0], ws[1]);
                    ensure!(
                        win.kernel[0] * win.kernel[1] == ws[2..].iter().product::<usize>(),
                        "Conv {} kernel_shape 與權重不符",
                        n.name
                    );
                    let taps = win.kernel[0] * win.kernel[1];
                    let bias = match konst(2) {
                        Some(b) => {
                            ensure!(b.len() == oc, "Conv bias 長度錯誤");
                            b.as_f32()?.to_vec()
                        }
                        None if ins.get(2).copied().flatten().is_none() => vec![0.0; oc],
                        None => bail!("Conv {} 的 bias 必須是常數", n.name),
                    };
                    Op::Conv {
                        win,
                        oc,
                        ic,
                        w: pack_conv_weights(w.as_f32()?, oc, ic, taps),
                        bias,
                    }
                }
                "MaxPool" => {
                    ensure!(n.outputs.len() == 1, "MaxPool 不支援 Indices 輸出");
                    Op::MaxPool(Window::from_node(n, None)?)
                }
                "MatMul" => Op::MatMul(match konst(1) {
                    Some(b) if b.shape.len() == 2 => {
                        Some(PackedB::new(b.as_f32()?, b.shape[0], b.shape[1]))
                    }
                    _ => None,
                }),
                "Gemm" => {
                    let trans_b = n.int("transB", 0) != 0;
                    let b = match konst(1) {
                        Some(b) => {
                            ensure!(b.shape.len() == 2, "Gemm B 必須是 2-D");
                            let (r, c) = (b.shape[0], b.shape[1]);
                            Some(if trans_b {
                                PackedB::new(&transpose2(b.as_f32()?, r, c), c, r)
                            } else {
                                PackedB::new(b.as_f32()?, r, c)
                            })
                        }
                        None => None,
                    };
                    Op::Gemm {
                        trans_a: n.int("transA", 0) != 0,
                        trans_b,
                        alpha: n.float("alpha", 1.0),
                        beta: n.float("beta", 1.0),
                        b,
                    }
                }
                "LayerNormalization" => Op::LayerNorm {
                    axis: n.int("axis", -1),
                    eps: n.float("epsilon", 1e-5),
                },
                "Softmax" => Op::Softmax(n.int("axis", -1)),
                op => bail!("不支援的 ONNX 運算子 {op}（節點 {}）", n.name),
            };
            let out = get_slot(out_name, &mut slot);
            steps.push(Step {
                op,
                name: format!("{}（{}）", n.name, n.op),
                ins,
                out,
                free: Vec::new(),
                inplace: false,
            });
        }
        let output = *slot
            .get(&g.outputs[0].name)
            .ok_or_else(|| anyhow!("找不到模型輸出 {}", g.outputs[0].name))?;
        // free intermediate values after their last use
        let keep: Vec<usize> = consts.iter().map(|c| c.0).chain([output]).collect();
        let mut last: HashMap<usize, usize> = HashMap::new();
        for (i, s) in steps.iter().enumerate() {
            for v in s.ins.iter().flatten() {
                last.insert(*v, i);
            }
        }
        for (v, i) in last {
            if !keep.contains(&v) {
                steps[i].free.push(v);
            }
        }
        for st in &mut steps {
            if let (Op::Unary(_) | Op::Binary(_) | Op::Clip { .. }, Some(Some(first))) =
                (&st.op, st.ins.first())
            {
                st.inplace = st.free.contains(first) && !st.ins[1..].contains(&Some(*first));
            }
        }
        let n_slots = slot.len();
        Ok(Self {
            steps,
            consts,
            n_slots,
            input,
            output,
            input_dims: g.inputs[0].dims.clone(),
            output_dims: g.outputs[0].dims.clone(),
            values: Vec::new(),
            scratch: Scratch::default(),
        })
    }

    /// Declared input dims (-1 = symbolic).
    pub fn input_dims(&self) -> &[i64] {
        &self.input_dims
    }

    /// Declared dims of the first output (-1 = symbolic).
    pub fn output_dims(&self) -> &[i64] {
        &self.output_dims
    }

    /// Run on one float input; returns the first output.
    pub fn run(&mut self, input: Tensor) -> Result<Arc<Tensor>> {
        self.values.clear();
        self.values.resize(self.n_slots, None);
        for (s, t) in &self.consts {
            self.values[*s] = Some(t.clone());
        }
        self.values[self.input] = Some(Arc::new(input));
        let Self {
            steps,
            values,
            scratch,
            ..
        } = self;
        for st in steps.iter() {
            // ops that cannot read channels-last data get it converted
            let scalar_rest = st.ins[1..]
                .iter()
                .flatten()
                .all(|s| values[*s].as_ref().is_some_and(|t| t.len() == 1));
            let aware = match st.op {
                Op::Conv { .. } | Op::MaxPool(_) | Op::Unary(_) | Op::Identity => true,
                Op::Binary(_) | Op::Clip { .. } => scalar_rest,
                _ => false,
            };
            if !aware {
                for s in st.ins.iter().flatten() {
                    if let Some(t) = values[*s].as_ref().filter(|t| t.nhwc) {
                        values[*s] = Some(Arc::new(to_nchw(t)?));
                    }
                }
            }
            // reuse the dying first input's buffer for elementwise ops
            let mut owned = None;
            if let (true, Some(Some(s))) = (st.inplace, st.ins.first()) {
                if let Some(arc) = values[*s].take() {
                    match Arc::try_unwrap(arc) {
                        Ok(t) => owned = Some(t),
                        Err(arc) => values[*s] = Some(arc),
                    }
                }
            }
            let skip = usize::from(owned.is_some());
            let mut ins: Vec<Option<&Tensor>> = st.ins[skip..]
                .iter()
                .map(|i| match i {
                    None => Ok(None),
                    Some(s) => values[*s]
                        .as_deref()
                        .map(Some)
                        .ok_or_else(|| anyhow!("值未計算")),
                })
                .collect::<Result<_>>()
                .with_context(|| st.name.clone())?;
            let y = match owned {
                Some(mut t) => {
                    if eval_inplace(&st.op, &mut t, &ins).with_context(|| st.name.clone())? {
                        t
                    } else {
                        ins.insert(0, Some(&t));
                        eval(&st.op, &ins, scratch).with_context(|| st.name.clone())?
                    }
                }
                None => eval(&st.op, &ins, scratch).with_context(|| st.name.clone())?,
            };
            values[st.out] = Some(Arc::new(y));
            for &f in &st.free {
                if let Some(Ok(Tensor {
                    data: Data::F32(v), ..
                })) = values[f].take().map(Arc::try_unwrap)
                {
                    scratch.give(v);
                }
            }
        }
        let mut out = self.values[self.output]
            .take()
            .ok_or_else(|| anyhow!("模型輸出未計算"))?;
        if out.nhwc {
            out = Arc::new(to_nchw(&out)?);
        }
        self.values.clear();
        Ok(out)
    }
}

fn transpose2(a: &[f32], r: usize, c: usize) -> Vec<f32> {
    let mut t = vec![0.0; r * c];
    for i in 0..r {
        for j in 0..c {
            t[j * r + i] = a[i * c + j];
        }
    }
    t
}

fn input<'a>(ins: &[Option<&'a Tensor>], i: usize) -> Result<&'a Tensor> {
    ins.get(i)
        .copied()
        .flatten()
        .ok_or_else(|| anyhow!("缺少第 {i} 個輸入"))
}

fn opt_scalar(ins: &[Option<&Tensor>], i: usize) -> Result<Option<f32>> {
    match ins.get(i).copied().flatten() {
        None => Ok(None),
        Some(t) => {
            ensure!(t.len() == 1, "預期純量");
            Ok(Some(match &t.data {
                Data::F32(v) => v[0],
                Data::I64(v) => v[0] as f32,
            }))
        }
    }
}

fn norm_axis(a: i64, rank: usize) -> Result<usize> {
    let r = rank as i64;
    let v = if a < 0 { a + r } else { a };
    ensure!((0..r.max(1)).contains(&v), "軸 {a} 超出範圍（rank {rank}）");
    Ok(v as usize)
}

fn strides(shape: &[usize]) -> Vec<usize> {
    let mut s = vec![1; shape.len()];
    for i in (0..shape.len().saturating_sub(1)).rev() {
        s[i] = s[i + 1] * shape[i + 1];
    }
    s
}

fn unary(u: Un, v: &mut [f32]) {
    fn map(v: &mut [f32], f: impl Fn(f32) -> f32) {
        v.iter_mut().for_each(|x| *x = f(*x));
    }
    match u {
        Un::Relu => map(v, |v| v.max(0.0)),
        Un::Sigmoid => map(v, |v| 1.0 / (1.0 + (-v).exp())),
        Un::Tanh => map(v, f32::tanh),
        Un::Exp => map(v, f32::exp),
        Un::Log => map(v, f32::ln),
        Un::Sqrt => map(v, f32::sqrt),
        Un::Neg => map(v, |v| -v),
        Un::Abs => map(v, f32::abs),
        Un::Reciprocal => map(v, |v| 1.0 / v),
        Un::Floor => map(v, f32::floor),
        Un::Ceil => map(v, f32::ceil),
        Un::LeakyRelu(a) => map(v, |v| if v >= 0.0 { v } else { a * v }),
    }
}

/// `a = f(a, b)` when `b` is a scalar or has `a`'s shape; false otherwise.
fn binary_inplace(op: Bin, a: &mut Tensor, b: &Tensor) -> Result<bool> {
    fn apply(a: &mut [f32], b: &[f32], f: impl Fn(f32, f32) -> f32) {
        if b.len() == 1 {
            let y = b[0];
            a.iter_mut().for_each(|x| *x = f(*x, y));
        } else {
            a.iter_mut().zip(b).for_each(|(x, &y)| *x = f(*x, y));
        }
    }
    if !(b.len() == 1 && b.shape.len() <= a.shape.len() || b.shape == a.shape) {
        return Ok(false);
    }
    let bv = b.as_f32()?;
    let Data::F32(av) = &mut a.data else {
        return Ok(false);
    };
    match op {
        Bin::Add => apply(av, bv, |a, b| a + b),
        Bin::Sub => apply(av, bv, |a, b| a - b),
        Bin::Mul => apply(av, bv, |a, b| a * b),
        Bin::Div => apply(av, bv, |a, b| a / b),
        Bin::Pow => apply(av, bv, pow),
        Bin::Max => apply(av, bv, max_nan),
        Bin::Min => apply(av, bv, min_nan),
    }
    Ok(true)
}

fn pow(a: f32, b: f32) -> f32 {
    if b == 2.0 {
        a * a
    } else if b == 1.0 {
        a
    } else {
        a.powf(b)
    }
}

/// NaN-propagating like ONNX Runtime
fn max_nan(a: f32, b: f32) -> f32 {
    if a.is_nan() || a >= b {
        a
    } else {
        b
    }
}

fn min_nan(a: f32, b: f32) -> f32 {
    if a.is_nan() || a <= b {
        a
    } else {
        b
    }
}

/// Evaluate `op` writing into its (owned, dying) first input `x`; `rest` are
/// the other inputs.  Returns false when the op cannot run in place.
fn eval_inplace(op: &Op, x: &mut Tensor, rest: &[Option<&Tensor>]) -> Result<bool> {
    match op {
        Op::Unary(u) => {
            let Data::F32(v) = &mut x.data else {
                bail!("預期 float 張量");
            };
            unary(*u, v);
            Ok(true)
        }
        Op::Binary(b) if rest.len() == 1 => binary_inplace(*b, x, input(rest, 0)?),
        Op::Clip { min, max } => {
            let lo = min.or(opt_scalar(rest, 0)?);
            let hi = max.or(opt_scalar(rest, 1)?);
            let Data::F32(v) = &mut x.data else {
                bail!("預期 float 張量");
            };
            for v in v.iter_mut() {
                if let Some(lo) = lo {
                    if *v < lo {
                        *v = lo;
                    }
                }
                if let Some(hi) = hi {
                    if *v > hi {
                        *v = hi;
                    }
                }
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn eval(op: &Op, ins: &[Option<&Tensor>], scratch: &mut Scratch) -> Result<Tensor> {
    let x = input(ins, 0)?;
    Ok(match op {
        Op::Identity => x.clone(),
        // channels-last input only with scalar other operands (see `run`)
        Op::Binary(_) | Op::Clip { .. } if x.nhwc => {
            let mut y = x.clone();
            ensure!(eval_inplace(op, &mut y, &ins[1..])?, "channels-last 運算失敗");
            y
        }
        Op::Unary(u) => {
            let mut v = x.as_f32()?.to_vec();
            unary(*u, &mut v);
            Tensor {
                shape: x.shape.clone(),
                data: Data::F32(v),
                nhwc: x.nhwc,
            }
        }
        Op::Binary(b) => {
            let mut acc = binary(*b, x, input(ins, 1)?)?;
            for i in 2..ins.len() {
                acc = binary(*b, &acc, input(ins, i)?)?;
            }
            acc
        }
        Op::Clip { min, max } => {
            let lo = min.or(opt_scalar(ins, 1)?);
            let hi = max.or(opt_scalar(ins, 2)?);
            let v = x.as_f32()?.iter().map(|&v| {
                let v = lo.map_or(v, |lo| if v < lo { lo } else { v });
                hi.map_or(v, |hi| if v > hi { hi } else { v })
            });
            Tensor::f32(x.shape.clone(), v.collect())
        }
        Op::Cast(to) => match (*to, &x.data) {
            (1, Data::F32(_)) | (7, Data::I64(_)) => x.clone(),
            (1, Data::I64(v)) => Tensor::f32(x.shape.clone(), v.iter().map(|&i| i as f32).collect()),
            (_, Data::F32(v)) => Tensor::i64(x.shape.clone(), v.iter().map(|&f| f as i64).collect()),
            _ => bail!("Cast to {to}"),
        },
        Op::Unsqueeze(axes) => {
            let axes = match axes {
                Some(a) => a.clone(),
                None => input(ins, 1)?.to_i64(),
            };
            let rank = x.shape.len() + axes.len();
            let mut ax = axes
                .iter()
                .map(|&a| norm_axis(a, rank))
                .collect::<Result<Vec<_>>>()?;
            ax.sort_unstable();
            let mut shape = x.shape.clone();
            for a in ax {
                ensure!(a <= shape.len(), "Unsqueeze 軸錯誤");
                shape.insert(a, 1);
            }
            Tensor {
                shape,
                data: x.data.clone(),
                nhwc: false,
            }
        }
        Op::Squeeze(axes) => {
            let axes = match (axes, ins.get(1).copied().flatten()) {
                (Some(a), _) => Some(a.clone()),
                (None, Some(t)) => Some(t.to_i64()),
                (None, None) => None,
            };
            let rank = x.shape.len();
            let drop: Vec<usize> = match axes {
                Some(a) => a.iter().map(|&a| norm_axis(a, rank)).collect::<Result<_>>()?,
                None => (0..rank).filter(|&i| x.shape[i] == 1).collect(),
            };
            let shape = (0..rank)
                .filter(|i| !drop.contains(i))
                .map(|i| x.shape[i])
                .collect();
            Tensor {
                shape,
                data: x.data.clone(),
                nhwc: false,
            }
        }
        Op::Reshape { allowzero } => {
            let spec = input(ins, 1)?.to_i64();
            let mut shape = Vec::with_capacity(spec.len());
            let mut infer = None;
            for (i, &d) in spec.iter().enumerate() {
                match d {
                    -1 => {
                        ensure!(infer.is_none(), "Reshape 有多個 -1");
                        infer = Some(i);
                        shape.push(1);
                    }
                    0 if !allowzero => shape.push(
                        *x.shape
                            .get(i)
                            .ok_or_else(|| anyhow!("Reshape 0 超出輸入維度"))?,
                    ),
                    d if d >= 0 => shape.push(d as usize),
                    d => bail!("Reshape 維度 {d}"),
                }
            }
            if let Some(i) = infer {
                let rest: usize = shape.iter().product();
                ensure!(rest > 0 && x.len() % rest == 0, "Reshape 無法推算 -1");
                shape[i] = x.len() / rest;
            }
            ensure!(
                shape.iter().product::<usize>() == x.len(),
                "Reshape {:?} -> {shape:?}",
                x.shape
            );
            Tensor {
                shape,
                data: x.data.clone(),
                nhwc: false,
            }
        }
        Op::Flatten(axis) => {
            let a = if *axis < 0 {
                norm_axis(*axis, x.shape.len())?
            } else {
                (*axis as usize).min(x.shape.len())
            };
            let outer: usize = x.shape[..a].iter().product();
            Tensor {
                shape: vec![outer, x.len() / outer.max(1)],
                data: x.data.clone(),
                nhwc: false,
            }
        }
        Op::Transpose(perm) => {
            let rank = x.shape.len();
            let perm: Vec<usize> = match perm {
                Some(p) => p
                    .iter()
                    .map(|&a| norm_axis(a, rank))
                    .collect::<Result<_>>()?,
                None => (0..rank).rev().collect(),
            };
            ensure!(perm.len() == rank, "Transpose perm 長度錯誤");
            transpose(x, &perm)?
        }
        Op::Reduce {
            kind,
            axes,
            keepdims,
            noop_empty,
        } => {
            let axes = match (axes, ins.get(1).copied().flatten()) {
                (Some(a), _) => a.clone(),
                (None, Some(t)) => t.to_i64(),
                (None, None) => Vec::new(),
            };
            if axes.is_empty() && *noop_empty {
                return Ok(x.clone());
            }
            reduce(x, *kind, &axes, *keepdims)?
        }
        Op::Conv {
            win,
            oc,
            ic,
            w,
            bias,
        } => conv(x, win, *oc, *ic, w, bias, scratch)?,
        Op::MaxPool(win) => maxpool(x, win, scratch)?,
        Op::MatMul(packed) => matmul(x, input(ins, 1)?, packed.as_ref(), scratch)?,
        Op::Gemm {
            trans_a,
            trans_b,
            alpha,
            beta,
            b,
        } => {
            ensure!(x.shape.len() == 2, "Gemm A 必須是 2-D");
            let (r, c) = (x.shape[0], x.shape[1]);
            let a: Vec<f32> = if *trans_a {
                transpose2(x.as_f32()?, r, c)
            } else {
                x.as_f32()?.to_vec()
            };
            let (m, k) = if *trans_a { (c, r) } else { (r, c) };
            let dynb;
            let pb = match b {
                Some(b) => b,
                None => {
                    let bt = input(ins, 1)?;
                    ensure!(bt.shape.len() == 2, "Gemm B 必須是 2-D");
                    let (br, bc) = (bt.shape[0], bt.shape[1]);
                    dynb = if *trans_b {
                        PackedB::new(&transpose2(bt.as_f32()?, br, bc), bc, br)
                    } else {
                        PackedB::new(bt.as_f32()?, br, bc)
                    };
                    &dynb
                }
            };
            ensure!(pb.k == k, "Gemm 內維度不符 {k} vs {}", pb.k);
            let mut y = gemm::matmul_b(&a, m, pb, scratch);
            if *alpha != 1.0 {
                y.iter_mut().for_each(|v| *v *= alpha);
            }
            let y = Tensor::f32(vec![m, pb.n], y);
            match ins.get(2).copied().flatten() {
                Some(cb) => {
                    let cb = if *beta != 1.0 {
                        Tensor::f32(
                            cb.shape.clone(),
                            cb.as_f32()?.iter().map(|v| v * beta).collect(),
                        )
                    } else {
                        cb.clone()
                    };
                    let z = binary(Bin::Add, &y, &cb)?;
                    ensure!(z.shape == y.shape, "Gemm C 形狀錯誤");
                    z
                }
                None => y,
            }
        }
        Op::LayerNorm { axis, eps } => {
            let a = norm_axis(*axis, x.shape.len())?;
            let inner: usize = x.shape[a..].iter().product();
            let scale = input(ins, 1)?.as_f32()?;
            let bias = match ins.get(2).copied().flatten() {
                Some(b) => Some(b.as_f32()?),
                None => None,
            };
            let pick = |v: &[f32], i: usize| if v.len() == 1 { v[0] } else { v[i] };
            ensure!(
                (scale.len() == inner || scale.len() == 1)
                    && bias.is_none_or(|b| b.len() == inner || b.len() == 1),
                "LayerNormalization scale / bias 形狀錯誤"
            );
            let mut out = Vec::with_capacity(x.len());
            for row in x.as_f32()?.chunks_exact(inner.max(1)) {
                let mean = row.iter().sum::<f32>() / inner as f32;
                let var = row.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / inner as f32;
                let inv = 1.0 / (var + eps).sqrt();
                out.extend(row.iter().enumerate().map(|(i, v)| {
                    (v - mean) * inv * pick(scale, i) + bias.map_or(0.0, |b| pick(b, i))
                }));
            }
            Tensor::f32(x.shape.clone(), out)
        }
        Op::Softmax(axis) => {
            // opset >= 13 semantics: along one axis
            let a = norm_axis(*axis, x.shape.len())?;
            let n = x.shape[a];
            let inner: usize = x.shape[a + 1..].iter().product();
            let v = x.as_f32()?;
            let mut out = v.to_vec();
            for outer in 0..x.len() / (n * inner).max(1) {
                for j in 0..inner {
                    let idx = |i: usize| outer * n * inner + i * inner + j;
                    let m = (0..n).map(|i| v[idx(i)]).fold(f32::NEG_INFINITY, f32::max);
                    let s: f32 = (0..n).map(|i| (v[idx(i)] - m).exp()).sum();
                    for i in 0..n {
                        out[idx(i)] = (v[idx(i)] - m).exp() / s;
                    }
                }
            }
            Tensor::f32(x.shape.clone(), out)
        }
    })
}

/// Numpy-style broadcasting binary op on float tensors.
fn binary(op: Bin, a: &Tensor, b: &Tensor) -> Result<Tensor> {
    match op {
        Bin::Add => broadcast(a, b, |a, b| a + b),
        Bin::Sub => broadcast(a, b, |a, b| a - b),
        Bin::Mul => broadcast(a, b, |a, b| a * b),
        Bin::Div => broadcast(a, b, |a, b| a / b),
        Bin::Pow => broadcast(a, b, pow),
        Bin::Max => broadcast(a, b, max_nan),
        Bin::Min => broadcast(a, b, min_nan),
    }
}

fn broadcast(a: &Tensor, b: &Tensor, f: impl Fn(f32, f32) -> f32) -> Result<Tensor> {
    let (av, bv) = (a.as_f32()?, b.as_f32()?);
    if a.shape == b.shape {
        return Ok(Tensor::f32(
            a.shape.clone(),
            av.iter().zip(bv).map(|(&x, &y)| f(x, y)).collect(),
        ));
    }
    let rank = a.shape.len().max(b.shape.len());
    let pad = |s: &[usize]| -> Vec<usize> {
        let mut v = vec![1; rank - s.len()];
        v.extend_from_slice(s);
        v
    };
    let (sa, sb) = (pad(&a.shape), pad(&b.shape));
    let mut shape = Vec::with_capacity(rank);
    for (&x, &y) in sa.iter().zip(&sb) {
        ensure!(
            x == y || x == 1 || y == 1,
            "無法廣播 {:?} 與 {:?}",
            a.shape,
            b.shape
        );
        shape.push(if x == 1 { y } else { x });
    }
    let n: usize = shape.iter().product();
    if bv.len() == 1 {
        let y = bv[0];
        return Ok(Tensor::f32(shape, av.iter().map(|&x| f(x, y)).collect()));
    }
    if av.len() == 1 {
        let x = av[0];
        return Ok(Tensor::f32(shape, bv.iter().map(|&y| f(x, y)).collect()));
    }
    let bstr = |s: &[usize]| -> Vec<usize> {
        let st = strides(s);
        s.iter()
            .zip(&shape)
            .zip(st)
            .map(|((&d, &o), st)| if d == 1 && o != 1 { 0 } else { st })
            .collect()
    };
    let (ta, tb) = (bstr(&sa), bstr(&sb));
    let mut out = Vec::with_capacity(n);
    if rank == 0 || n == 0 {
        return Ok(Tensor::f32(shape, out));
    }
    let last = rank - 1;
    let inner = shape[last];
    let mut idx = vec![0usize; last];
    loop {
        let oa: usize = idx.iter().zip(&ta).map(|(i, s)| i * s).sum();
        let ob: usize = idx.iter().zip(&tb).map(|(i, s)| i * s).sum();
        for i in 0..inner {
            out.push(f(av[oa + i * ta[last]], bv[ob + i * tb[last]]));
        }
        // advance the outer index
        let mut d = last;
        loop {
            if d == 0 {
                return Ok(Tensor::f32(shape, out));
            }
            d -= 1;
            idx[d] += 1;
            if idx[d] < shape[d] {
                break;
            }
            idx[d] = 0;
        }
    }
}

fn transpose(x: &Tensor, perm: &[usize]) -> Result<Tensor> {
    let rank = x.shape.len();
    let st = strides(&x.shape);
    let shape: Vec<usize> = perm.iter().map(|&p| x.shape[p]).collect();
    let pst: Vec<usize> = perm.iter().map(|&p| st[p]).collect();
    let n = x.len();
    let mut idx = vec![0usize; rank];
    let mut offs = Vec::with_capacity(n);
    for _ in 0..n {
        offs.push(idx.iter().zip(&pst).map(|(i, s)| i * s).sum::<usize>());
        for d in (0..rank).rev() {
            idx[d] += 1;
            if idx[d] < shape[d] {
                break;
            }
            idx[d] = 0;
        }
    }
    Ok(match &x.data {
        Data::F32(v) => Tensor::f32(shape, offs.iter().map(|&o| v[o]).collect()),
        Data::I64(v) => Tensor::i64(shape, offs.iter().map(|&o| v[o]).collect()),
    })
}

fn reduce(x: &Tensor, kind: Red, axes: &[i64], keepdims: bool) -> Result<Tensor> {
    let rank = x.shape.len();
    let mut red = vec![axes.is_empty(); rank];
    for &a in axes {
        red[norm_axis(a, rank)?] = true;
    }
    let out_full: Vec<usize> = (0..rank)
        .map(|i| if red[i] { 1 } else { x.shape[i] })
        .collect();
    let ost = strides(&out_full);
    let n_out: usize = out_full.iter().product();
    let count = (x.len() / n_out.max(1)).max(1);
    let init = match kind {
        Red::Max => f32::NEG_INFINITY,
        Red::Min => f32::INFINITY,
        Red::Mean | Red::Sum => 0.0,
    };
    let mut out = vec![init; n_out];
    let v = x.as_f32()?;
    let mut idx = vec![0usize; rank];
    for &val in v {
        let o: usize = (0..rank).map(|d| if red[d] { 0 } else { idx[d] * ost[d] }).sum();
        let r = &mut out[o];
        *r = match kind {
            Red::Max => r.max(val),
            Red::Min => r.min(val),
            Red::Mean | Red::Sum => *r + val,
        };
        for d in (0..rank).rev() {
            idx[d] += 1;
            if idx[d] < x.shape[d] {
                break;
            }
            idx[d] = 0;
        }
    }
    if let Red::Mean = kind {
        out.iter_mut().for_each(|v| *v /= count as f32);
    }
    let shape = if keepdims {
        out_full
    } else {
        (0..rank).filter(|&i| !red[i]).map(|i| x.shape[i]).collect()
    };
    Ok(Tensor::f32(shape, out))
}

/// ONNX conv weights `[oc, ic, taps]` as a packed `(taps * ic) x oc` GEMM
/// operand with rows ordered (tap, ci), matching the channels-last im2col.
fn pack_conv_weights(w: &[f32], oc: usize, ic: usize, taps: usize) -> PackedB {
    PackedB::from_fn(taps * ic, oc, |r, o| {
        let (tap, ci) = (r / ic, r % ic);
        w[(o * ic + ci) * taps + tap]
    })
}

/// Data of a logical `[N, C, spatial..]` tensor in channels-last order.
fn channels_last(x: &Tensor) -> Result<std::borrow::Cow<'_, [f32]>> {
    let v = x.as_f32()?;
    let (nb, c) = (x.shape[0], x.shape[1]);
    let sp: usize = x.shape[2..].iter().product();
    if x.nhwc || c == 1 || sp == 1 {
        return Ok(v.into());
    }
    let mut out = Vec::with_capacity(v.len());
    for b in v.chunks_exact((c * sp).max(1)).take(nb) {
        for p in 0..sp {
            out.extend((0..c).map(|ci| b[ci * sp + p]));
        }
    }
    Ok(out.into())
}

/// A channels-last tensor back in the standard (NCHW) layout.
fn to_nchw(x: &Tensor) -> Result<Tensor> {
    let v = x.as_f32()?;
    let (nb, c) = (x.shape[0], x.shape[1]);
    let sp: usize = x.shape[2..].iter().product();
    let data = if c == 1 || sp == 1 {
        v.to_vec()
    } else {
        let mut out = Vec::with_capacity(v.len());
        for b in v.chunks_exact((c * sp).max(1)).take(nb) {
            for ci in 0..c {
                out.extend((0..sp).map(|p| b[p * c + ci]));
            }
        }
        out
    };
    Ok(Tensor::f32(x.shape.clone(), data))
}

/// Conv as im2col + GEMM on channels-last data (one batch item at a time);
/// the result is channels-last.
fn conv(
    x: &Tensor,
    win: &Window,
    oc: usize,
    ic: usize,
    w: &PackedB,
    bias: &[f32],
    scratch: &mut Scratch,
) -> Result<Tensor> {
    let [nb, c, h, wd] = win.nchw(&x.shape)?;
    ensure!(c == ic, "Conv 輸入通道 {c}，預期 {ic}");
    let (oh, pt) = win.out_dim(0, h)?;
    let (ow, pl) = win.out_dim(1, wd)?;
    let [kh, kw] = win.kernel;
    let [sh, sw] = win.strides;
    let [dh, dw] = win.dilations;
    let n = oh * ow;
    let k = kh * kw * c;
    ensure!(k == w.k && oc == w.n, "Conv 權重形狀不符");
    let xl = channels_last(x)?;
    let mut out = scratch.take(nb * n * oc);
    for xb in xl.chunks_exact((h * wd * c).max(1)).take(nb) {
        let (a, cbuf) = scratch.bufs(n * k, n * w.np);
        // im2col: row p = output position, (ky, kx, ci) taps
        for oy in 0..oh {
            for ox in 0..ow {
                let row = &mut a[(oy * ow + ox) * k..][..k];
                let ix0 = (ox * sw) as isize - pl as isize;
                for ky in 0..kh {
                    let seg = &mut row[ky * kw * c..(ky + 1) * kw * c];
                    let iy = (oy * sh + ky * dh) as isize - pt as isize;
                    if iy < 0 || iy >= h as isize {
                        seg.fill(0.0);
                        continue;
                    }
                    let base = iy as usize * wd;
                    if dw == 1 && ix0 >= 0 && ix0 as usize + kw <= wd {
                        let s0 = (base + ix0 as usize) * c;
                        seg.copy_from_slice(&xb[s0..s0 + kw * c]);
                        continue;
                    }
                    for (kx, d) in seg.chunks_exact_mut(c).enumerate() {
                        let ix = ix0 + (kx * dw) as isize;
                        if ix < 0 || ix >= wd as isize {
                            d.fill(0.0);
                        } else {
                            let s0 = (base + ix as usize) * c;
                            d.copy_from_slice(&xb[s0..s0 + c]);
                        }
                    }
                }
            }
        }
        gemm::gemm(a, n, w, cbuf);
        for row in cbuf.chunks_exact(w.np).take(n) {
            out.extend(row[..oc].iter().zip(bias).map(|(v, b)| v + b));
        }
    }
    let shape = if win.one_d {
        vec![nb, oc, ow]
    } else {
        vec![nb, oc, oh, ow]
    };
    Ok(Tensor {
        shape,
        data: Data::F32(out),
        nhwc: true,
    })
}

/// MaxPool on channels-last data; the result is channels-last.
fn maxpool(x: &Tensor, win: &Window, scratch: &mut Scratch) -> Result<Tensor> {
    let [nb, c, h, wd] = win.nchw(&x.shape)?;
    let (oh, pt) = win.out_dim(0, h)?;
    let (ow, pl) = win.out_dim(1, wd)?;
    let [kh, kw] = win.kernel;
    let [sh, sw] = win.strides;
    let [dh, dw] = win.dilations;
    let xl = channels_last(x)?;
    let mut out = scratch.take(nb * c * oh * ow);
    for xb in xl.chunks_exact((h * wd * c).max(1)).take(nb) {
        for oy in 0..oh {
            for ox in 0..ow {
                let o0 = out.len();
                out.resize(o0 + c, f32::NEG_INFINITY);
                let dst = &mut out[o0..];
                for ky in 0..kh {
                    let iy = (oy * sh + ky * dh) as isize - pt as isize;
                    if iy < 0 || iy >= h as isize {
                        continue;
                    }
                    for kx in 0..kw {
                        let ix = (ox * sw + kx * dw) as isize - pl as isize;
                        if ix < 0 || ix >= wd as isize {
                            continue;
                        }
                        let s0 = (iy as usize * wd + ix as usize) * c;
                        for (d, &v) in dst.iter_mut().zip(&xb[s0..s0 + c]) {
                            *d = max_nan(v, *d);
                        }
                    }
                }
            }
        }
    }
    let shape = if win.one_d {
        vec![nb, c, ow]
    } else {
        vec![nb, c, oh, ow]
    };
    Ok(Tensor {
        shape,
        data: Data::F32(out),
        nhwc: true,
    })
}

/// numpy `matmul` for A of rank >= 1 and B of rank 1 or 2, or equal batch
/// dims.
fn matmul(
    a: &Tensor,
    b: &Tensor,
    packed: Option<&PackedB>,
    scratch: &mut Scratch,
) -> Result<Tensor> {
    let av = a.as_f32()?;
    let a_shape = if a.shape.len() == 1 {
        vec![1, a.shape[0]]
    } else {
        a.shape.clone()
    };
    let (b_shape, b_vec) = if b.shape.len() == 1 {
        (vec![b.shape[0], 1], true)
    } else {
        (b.shape.clone(), false)
    };
    let ar = a_shape.len();
    let (m, k) = (a_shape[ar - 2], a_shape[ar - 1]);
    let br = b_shape.len();
    let (kb, n) = (b_shape[br - 2], b_shape[br - 1]);
    ensure!(k == kb, "MatMul 內維度不符 {:?} x {:?}", a.shape, b.shape);
    let mut shape: Vec<usize>;
    let out: Vec<f32>;
    if br == 2 {
        // fold all of A's leading dims into M
        let rows = av.len() / k.max(1);
        let dynb;
        let pb = match packed {
            Some(p) => p,
            None => {
                dynb = PackedB::new(b.as_f32()?, kb, n);
                &dynb
            }
        };
        out = gemm::matmul_b(av, rows, pb, scratch);
        shape = a_shape[..ar - 2].to_vec();
        shape.extend([m, n]);
    } else {
        let batch_a = &a_shape[..ar - 2];
        let batch_b = &b_shape[..br - 2];
        ensure!(
            batch_a == batch_b || ar == 2,
            "MatMul 不支援的批次廣播 {:?} x {:?}",
            a.shape,
            b.shape
        );
        let nbatch: usize = batch_b.iter().product();
        let bv = b.as_f32()?;
        let mut o = Vec::with_capacity(nbatch * m * n);
        for i in 0..nbatch {
            let ai = if ar == 2 { 0 } else { i };
            let pb = PackedB::new(&bv[i * k * n..(i + 1) * k * n], k, n);
            o.extend(gemm::matmul_b(
                &av[ai * m * k..(ai + 1) * m * k],
                m,
                &pb,
                scratch,
            ));
        }
        out = o;
        shape = batch_b.to_vec();
        shape.extend([m, n]);
    }
    if a.shape.len() == 1 {
        shape.remove(shape.len() - 2);
    }
    if b_vec {
        shape.pop();
    }
    Ok(Tensor::f32(shape, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(shape: &[usize], v: &[f32]) -> Tensor {
        Tensor::f32(shape.to_vec(), v.to_vec())
    }

    #[test]
    fn broadcasting() {
        let a = t(&[2, 3], &[1., 2., 3., 4., 5., 6.]);
        let b = t(&[3], &[10., 20., 30.]);
        let c = t(&[2, 1], &[100., 200.]);
        assert_eq!(
            binary(Bin::Add, &a, &b).unwrap().as_f32().unwrap(),
            [11., 22., 33., 14., 25., 36.]
        );
        let r = binary(Bin::Sub, &c, &b).unwrap();
        assert_eq!(r.shape, [2, 3]);
        assert_eq!(r.as_f32().unwrap(), [90., 80., 70., 190., 180., 170.]);
        let s = binary(Bin::Max, &a, &t(&[1, 1], &[3.5])).unwrap();
        assert_eq!(s.as_f32().unwrap(), [3.5, 3.5, 3.5, 4., 5., 6.]);
    }

    #[test]
    fn transpose_and_reduce() {
        let a = t(&[2, 3], &[1., 2., 3., 4., 5., 6.]);
        let tr = transpose(&a, &[1, 0]).unwrap();
        assert_eq!(tr.shape, [3, 2]);
        assert_eq!(tr.as_f32().unwrap(), [1., 4., 2., 5., 3., 6.]);
        let m = reduce(&a, Red::Mean, &[-1], true).unwrap();
        assert_eq!(m.shape, [2, 1]);
        assert_eq!(m.as_f32().unwrap(), [2., 5.]);
        let mx = reduce(&a, Red::Max, &[], false).unwrap();
        assert_eq!(mx.shape, Vec::<usize>::new());
        assert_eq!(mx.as_f32().unwrap(), [6.]);
    }

    /// Direct (naive) 2-D convolution for checking im2col + GEMM.
    #[allow(clippy::too_many_arguments)]
    fn naive_conv(
        x: &[f32],
        [c, h, w]: [usize; 3],
        wt: &[f32],
        oc: usize,
        [kh, kw]: [usize; 2],
        [sh, sw]: [usize; 2],
        [pt, pl]: [usize; 2],
        [oh, ow]: [usize; 2],
    ) -> Vec<f32> {
        let mut out = vec![0.0; oc * oh * ow];
        for o in 0..oc {
            for oy in 0..oh {
                for ox in 0..ow {
                    let mut s = 0.0f64;
                    for ci in 0..c {
                        for ky in 0..kh {
                            for kx in 0..kw {
                                let iy = (oy * sh + ky) as isize - pt as isize;
                                let ix = (ox * sw + kx) as isize - pl as isize;
                                if iy >= 0 && ix >= 0 && (iy as usize) < h && (ix as usize) < w {
                                    s += (x[ci * h * w + iy as usize * w + ix as usize]
                                        * wt[((o * c + ci) * kh + ky) * kw + kx])
                                        as f64;
                                }
                            }
                        }
                    }
                    out[(o * oh + oy) * ow + ox] = s as f32;
                }
            }
        }
        out
    }

    #[test]
    fn conv_matches_naive() {
        let mut s = Scratch::default();
        for &(c, h, w, oc, kh, kw, sh, sw, pt, pl) in &[
            (1, 9, 7, 5, 3, 3, 1, 1, 0, 1),
            (3, 8, 6, 7, 1, 3, 1, 1, 0, 1),
            (4, 10, 9, 8, 3, 1, 2, 3, 1, 0),
            (1, 1, 40, 9, 1, 8, 1, 5, 0, 0),
        ] {
            let x: Vec<f32> = (0..c * h * w).map(|i| ((i * 31 % 17) as f32) - 8.0).collect();
            let wt: Vec<f32> = (0..oc * c * kh * kw)
                .map(|i| ((i * 7 % 13) as f32 - 6.0) / 7.0)
                .collect();
            let win = Window {
                one_d: false,
                kernel: [kh, kw],
                strides: [sh, sw],
                dilations: [1, 1],
                pads: [pt, pl, pt, pl],
                auto_pad: AutoPad::NotSet,
                ceil_mode: false,
            };
            let y = conv(
                &t(&[1, c, h, w], &x),
                &win,
                oc,
                c,
                &pack_conv_weights(&wt, oc, c, kh * kw),
                &vec![0.5; oc],
                &mut s,
            )
            .unwrap();
            let (oh, ow) = (y.shape[2], y.shape[3]);
            assert_eq!(oh, (h + 2 * pt - kh) / sh + 1);
            assert_eq!(ow, (w + 2 * pl - kw) / sw + 1);
            let want = naive_conv(
                &x,
                [c, h, w],
                &wt,
                oc,
                [kh, kw],
                [sh, sw],
                [pt, pl],
                [oh, ow],
            );
            assert!(y.nhwc);
            let y = to_nchw(&y).unwrap();
            for (g, w) in y.as_f32().unwrap().iter().zip(&want) {
                assert!((g - (w + 0.5)).abs() < 1e-4, "{g} vs {w}");
            }
        }
    }
}

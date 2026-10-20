//! Minimal ONNX model reader: just enough of the protobuf wire format to
//! read a `ModelProto`'s graph (nodes, attributes, initializers, input /
//! output shapes).  No external data, no sub-graphs.

use std::collections::HashMap;

use anyhow::{anyhow, bail, ensure, Result};

/// Element data of a tensor.
#[derive(Clone, Debug)]
pub enum Data {
    F32(Vec<f32>),
    I64(Vec<i64>),
}

#[derive(Clone, Debug)]
pub struct Tensor {
    /// logical (ONNX) shape
    pub shape: Vec<usize>,
    pub data: Data,
    /// memory is channels-last (`[N, spatial.., C]` for a logical
    /// `[N, C, spatial..]`); only produced by the interpreter's conv / pool
    pub nhwc: bool,
}

impl Tensor {
    pub fn f32(shape: Vec<usize>, v: Vec<f32>) -> Self {
        Self {
            shape,
            data: Data::F32(v),
            nhwc: false,
        }
    }

    pub fn i64(shape: Vec<usize>, v: Vec<i64>) -> Self {
        Self {
            shape,
            data: Data::I64(v),
            nhwc: false,
        }
    }

    pub fn len(&self) -> usize {
        self.shape.iter().product()
    }

    pub fn as_f32(&self) -> Result<&[f32]> {
        match &self.data {
            Data::F32(v) => Ok(v),
            Data::I64(_) => bail!("預期 float 張量，收到 int64"),
        }
    }

    /// Values as i64 (float values are truncated, for shape-like inputs).
    pub fn to_i64(&self) -> Vec<i64> {
        match &self.data {
            Data::F32(v) => v.iter().map(|&x| x as i64).collect(),
            Data::I64(v) => v.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Attr {
    Float(f32),
    Int(i64),
    Bytes(Vec<u8>),
    Tensor(Tensor),
    Floats(Vec<f32>),
    Ints(Vec<i64>),
}

#[derive(Debug)]
pub struct Node {
    pub op: String,
    pub domain: String,
    pub name: String,
    /// "" = optional input left out
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub attrs: HashMap<String, Attr>,
}

impl Node {
    pub fn int(&self, k: &str, default: i64) -> i64 {
        match self.attrs.get(k) {
            Some(Attr::Int(v)) => *v,
            _ => default,
        }
    }

    pub fn float(&self, k: &str, default: f32) -> f32 {
        match self.attrs.get(k) {
            Some(Attr::Float(v)) => *v,
            _ => default,
        }
    }

    pub fn ints(&self, k: &str) -> Option<Vec<i64>> {
        match self.attrs.get(k) {
            Some(Attr::Ints(v)) => Some(v.clone()),
            Some(Attr::Int(v)) => Some(vec![*v]),
            _ => None,
        }
    }

    pub fn string(&self, k: &str) -> Option<String> {
        match self.attrs.get(k) {
            Some(Attr::Bytes(v)) => Some(String::from_utf8_lossy(v).into_owned()),
            _ => None,
        }
    }
}

/// Graph input / output: name and dims (-1 for symbolic / unknown).
#[derive(Clone, Debug)]
pub struct ValueInfo {
    pub name: String,
    pub dims: Vec<i64>,
}

#[derive(Debug, Default)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub initializers: HashMap<String, Tensor>,
    pub inputs: Vec<ValueInfo>,
    pub outputs: Vec<ValueInfo>,
}

// ---- protobuf wire format ----

enum Wire<'a> {
    Varint(u64),
    I64(u64),
    Len(&'a [u8]),
    I32(u32),
}

struct Reader<'a> {
    b: &'a [u8],
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b }
    }

    fn varint(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for i in 0..10 {
            let (&byte, rest) = self
                .b
                .split_first()
                .ok_or_else(|| anyhow!("protobuf 資料被截斷"))?;
            self.b = rest;
            v |= u64::from(byte & 0x7f) << (7 * i);
            if byte & 0x80 == 0 {
                return Ok(v);
            }
        }
        bail!("protobuf varint 太長")
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        ensure!(n <= self.b.len(), "protobuf 資料被截斷");
        let (h, t) = self.b.split_at(n);
        self.b = t;
        Ok(h)
    }

    fn next(&mut self) -> Result<Option<(u64, Wire<'a>)>> {
        if self.b.is_empty() {
            return Ok(None);
        }
        let key = self.varint()?;
        let w = match key & 7 {
            0 => Wire::Varint(self.varint()?),
            1 => Wire::I64(u64::from_le_bytes(self.take(8)?.try_into()?)),
            2 => {
                let n = usize::try_from(self.varint()?)?;
                Wire::Len(self.take(n)?)
            }
            5 => Wire::I32(u32::from_le_bytes(self.take(4)?.try_into()?)),
            t => bail!("不支援的 protobuf wire type {t}"),
        };
        Ok(Some((key >> 3, w)))
    }
}

fn string(w: &Wire) -> Result<String> {
    match w {
        Wire::Len(b) => Ok(String::from_utf8_lossy(b).into_owned()),
        _ => bail!("預期字串欄位"),
    }
}

fn bytes<'a>(w: &Wire<'a>) -> Result<&'a [u8]> {
    match w {
        Wire::Len(b) => Ok(b),
        _ => bail!("預期 bytes 欄位"),
    }
}

/// A repeated varint field, packed or not.
fn push_varints(w: &Wire, out: &mut Vec<i64>) -> Result<()> {
    match w {
        Wire::Varint(v) => out.push(*v as i64),
        Wire::Len(b) => {
            let mut r = Reader::new(b);
            while !r.b.is_empty() {
                out.push(r.varint()? as i64);
            }
        }
        _ => bail!("預期整數欄位"),
    }
    Ok(())
}

/// A repeated float field, packed or not.
fn push_floats(w: &Wire, out: &mut Vec<f32>) -> Result<()> {
    match w {
        Wire::I32(v) => out.push(f32::from_bits(*v)),
        Wire::Len(b) => {
            ensure!(b.len() % 4 == 0, "float 陣列長度錯誤");
            out.extend(
                b.chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])),
            );
        }
        _ => bail!("預期 float 欄位"),
    }
    Ok(())
}

/// A repeated double field, packed or not.
fn push_doubles(w: &Wire, out: &mut Vec<f64>) -> Result<()> {
    match w {
        Wire::I64(v) => out.push(f64::from_bits(*v)),
        Wire::Len(b) => {
            ensure!(b.len() % 8 == 0, "double 陣列長度錯誤");
            out.extend(
                b.chunks_exact(8)
                    .map(|c| f64::from_le_bytes(c.try_into().unwrap_or_default())),
            );
        }
        _ => bail!("預期 double 欄位"),
    }
    Ok(())
}

// TensorProto.DataType
const FLOAT: i64 = 1;
const UINT8: i64 = 2;
const INT8: i64 = 3;
const INT32: i64 = 6;
const INT64: i64 = 7;
const BOOL: i64 = 9;
const DOUBLE: i64 = 11;

fn tensor(b: &[u8]) -> Result<(String, Tensor)> {
    let mut r = Reader::new(b);
    let (mut dims, mut dtype, mut name) = (Vec::new(), 0i64, String::new());
    let (mut floats, mut ints, mut doubles) = (Vec::new(), Vec::new(), Vec::new());
    let mut raw: Option<&[u8]> = None;
    while let Some((f, w)) = r.next()? {
        match f {
            1 => push_varints(&w, &mut dims)?,
            2 => {
                if let Wire::Varint(v) = w {
                    dtype = v as i64;
                }
            }
            4 => push_floats(&w, &mut floats)?,
            5 | 7 => push_varints(&w, &mut ints)?,
            8 => name = string(&w)?,
            9 => raw = Some(bytes(&w)?),
            10 => push_doubles(&w, &mut doubles)?,
            13 | 14 => bail!("不支援外部資料（external data）的 ONNX 張量 {name}"),
            _ => {}
        }
    }
    let shape = dims
        .iter()
        .map(|&d| usize::try_from(d).map_err(|_| anyhow!("張量 {name} 維度錯誤 {d}")))
        .collect::<Result<Vec<_>>>()?;
    let n: usize = shape.iter().product();
    let t = match dtype {
        FLOAT | DOUBLE => {
            let v: Vec<f32> = match (raw, dtype) {
                (Some(b), FLOAT) => {
                    let mut v = Vec::new();
                    push_floats(&Wire::Len(b), &mut v)?;
                    v
                }
                (Some(b), _) => {
                    let mut v = Vec::new();
                    push_doubles(&Wire::Len(b), &mut v)?;
                    v.into_iter().map(|x| x as f32).collect()
                }
                (None, FLOAT) => floats,
                (None, _) => doubles.into_iter().map(|x| x as f32).collect(),
            };
            Tensor::f32(shape, v)
        }
        INT64 | INT32 | INT8 | UINT8 | BOOL => {
            let v: Vec<i64> = match raw {
                Some(b) => match dtype {
                    INT64 => b
                        .chunks_exact(8)
                        .map(|c| i64::from_le_bytes(c.try_into().unwrap_or_default()))
                        .collect(),
                    INT32 => b
                        .chunks_exact(4)
                        .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as i64)
                        .collect(),
                    INT8 => b.iter().map(|&x| x as i8 as i64).collect(),
                    _ => b.iter().map(|&x| x as i64).collect(),
                },
                None if dtype == INT32 || dtype == INT8 => {
                    ints.iter().map(|&x| x as i32 as i64).collect()
                }
                None => ints,
            };
            Tensor::i64(shape, v)
        }
        t => bail!("不支援的 ONNX 張量型別 {t}（{name}）"),
    };
    ensure!(
        t.len() == n
            && match &t.data {
                Data::F32(v) => v.len() == n,
                Data::I64(v) => v.len() == n,
            },
        "張量 {name} 資料長度不符"
    );
    Ok((name, t))
}

fn attribute(b: &[u8]) -> Result<(String, Attr)> {
    let mut r = Reader::new(b);
    let mut name = String::new();
    let mut ty = 0u64;
    let (mut f, mut i, mut s, mut t) = (None, None, None, None);
    let (mut floats, mut ints) = (Vec::new(), Vec::new());
    while let Some((k, w)) = r.next()? {
        match k {
            1 => name = string(&w)?,
            20 => {
                if let Wire::Varint(v) = w {
                    ty = v;
                }
            }
            2 => {
                let mut v = Vec::new();
                push_floats(&w, &mut v)?;
                f = v.first().copied();
            }
            3 => {
                if let Wire::Varint(v) = w {
                    i = Some(v as i64);
                }
            }
            4 => s = Some(bytes(&w)?.to_vec()),
            5 => t = Some(tensor(bytes(&w)?)?.1),
            7 => push_floats(&w, &mut floats)?,
            8 => push_varints(&w, &mut ints)?,
            _ => {}
        }
    }
    // AttributeProto.AttributeType
    let a = match ty {
        1 => Attr::Float(f.unwrap_or(0.0)),
        2 => Attr::Int(i.unwrap_or(0)),
        3 => Attr::Bytes(s.unwrap_or_default()),
        4 => Attr::Tensor(t.ok_or_else(|| anyhow!("屬性 {name} 缺少張量"))?),
        6 => Attr::Floats(floats),
        7 => Attr::Ints(ints),
        // untyped (very old exporters): guess from what is set
        0 => match (f, i, s, t) {
            (Some(f), ..) => Attr::Float(f),
            (_, Some(i), ..) => Attr::Int(i),
            (_, _, Some(s), _) => Attr::Bytes(s),
            (_, _, _, Some(t)) => Attr::Tensor(t),
            _ if !floats.is_empty() => Attr::Floats(floats),
            _ => Attr::Ints(ints),
        },
        t => bail!("不支援的 ONNX 屬性型別 {t}（{name}）"),
    };
    Ok((name, a))
}

fn node(b: &[u8]) -> Result<Node> {
    let mut r = Reader::new(b);
    let mut n = Node {
        op: String::new(),
        domain: String::new(),
        name: String::new(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        attrs: HashMap::new(),
    };
    while let Some((k, w)) = r.next()? {
        match k {
            1 => n.inputs.push(string(&w)?),
            2 => n.outputs.push(string(&w)?),
            3 => n.name = string(&w)?,
            4 => n.op = string(&w)?,
            5 => {
                let (k, a) = attribute(bytes(&w)?)?;
                n.attrs.insert(k, a);
            }
            7 => n.domain = string(&w)?,
            _ => {}
        }
    }
    Ok(n)
}

fn value_info(b: &[u8]) -> Result<ValueInfo> {
    let mut r = Reader::new(b);
    let mut v = ValueInfo {
        name: String::new(),
        dims: Vec::new(),
    };
    while let Some((k, w)) = r.next()? {
        match k {
            1 => v.name = string(&w)?,
            2 => {
                // TypeProto.tensor_type(1).shape(2).dim(1).dim_value(1)
                let mut tp = Reader::new(bytes(&w)?);
                while let Some((k, w)) = tp.next()? {
                    if k != 1 {
                        continue;
                    }
                    let mut tt = Reader::new(bytes(&w)?);
                    while let Some((k, w)) = tt.next()? {
                        if k != 2 {
                            continue;
                        }
                        let mut sh = Reader::new(bytes(&w)?);
                        while let Some((k, w)) = sh.next()? {
                            if k != 1 {
                                continue;
                            }
                            let mut d = Reader::new(bytes(&w)?);
                            let mut val = -1;
                            while let Some((k, w)) = d.next()? {
                                if let (1, Wire::Varint(x)) = (k, w) {
                                    val = x as i64;
                                }
                            }
                            v.dims.push(val);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(v)
}

fn graph(b: &[u8]) -> Result<Graph> {
    let mut r = Reader::new(b);
    let mut g = Graph::default();
    let mut inputs = Vec::new();
    while let Some((k, w)) = r.next()? {
        match k {
            1 => g.nodes.push(node(bytes(&w)?)?),
            5 => {
                let (name, t) = tensor(bytes(&w)?)?;
                g.initializers.insert(name, t);
            }
            11 => inputs.push(value_info(bytes(&w)?)?),
            12 => g.outputs.push(value_info(bytes(&w)?)?),
            15 => bail!("不支援稀疏（sparse）初始值"),
            _ => {}
        }
    }
    // older exporters also list initializers as graph inputs
    g.inputs = inputs
        .into_iter()
        .filter(|i| !g.initializers.contains_key(&i.name))
        .collect();
    Ok(g)
}

/// Parse an ONNX `ModelProto`.
pub fn parse_model(b: &[u8]) -> Result<Graph> {
    let mut r = Reader::new(b);
    let mut g = None;
    while let Some((k, w)) = r.next()? {
        if k == 7 {
            g = Some(graph(bytes(&w)?)?);
        }
    }
    g.ok_or_else(|| anyhow!("不是 ONNX 模型（找不到 graph）"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }

    fn len_field(k: u64, body: &[u8], out: &mut Vec<u8>) {
        varint(k << 3 | 2, out);
        varint(body.len() as u64, out);
        out.extend_from_slice(body);
    }

    #[test]
    fn packed_and_unpacked_fields() {
        // TensorProto: dims 2,3 (unpacked) data_type FLOAT, float_data packed
        let mut t = Vec::new();
        for d in [2u64, 3] {
            varint(1 << 3, &mut t);
            varint(d, &mut t);
        }
        varint(2 << 3, &mut t);
        varint(1, &mut t);
        let data: Vec<u8> = (0..6).flat_map(|i| (i as f32).to_le_bytes()).collect();
        len_field(4, &data, &mut t);
        len_field(8, b"w", &mut t);
        let (name, x) = tensor(&t).unwrap();
        assert_eq!(name, "w");
        assert_eq!(x.shape, [2, 3]);
        assert_eq!(x.as_f32().unwrap(), [0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);

        // int64 raw_data with negative values
        let mut t = Vec::new();
        varint(1 << 3, &mut t);
        varint(2, &mut t);
        varint(2 << 3, &mut t);
        varint(7, &mut t);
        let raw: Vec<u8> = [-1i64, 5].iter().flat_map(|v| v.to_le_bytes()).collect();
        len_field(9, &raw, &mut t);
        assert_eq!(tensor(&t).unwrap().1.to_i64(), [-1, 5]);

        // packed negative ints attribute
        let mut a = Vec::new();
        len_field(1, b"pads", &mut a);
        let mut p = Vec::new();
        for v in [0i64, -1, 3] {
            varint(v as u64, &mut p);
        }
        len_field(8, &p, &mut a);
        varint(20 << 3, &mut a);
        varint(7, &mut a);
        let (k, v) = attribute(&a).unwrap();
        assert_eq!(k, "pads");
        assert!(matches!(v, Attr::Ints(ref x) if x == &[0, -1, 3]));
    }

    #[test]
    fn truncated_input_is_an_error() {
        assert!(parse_model(&[0x3a, 0x05, 0x01]).is_err());
        assert!(parse_model(b"").is_err());
    }
}

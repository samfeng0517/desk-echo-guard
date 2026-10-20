"""Build a small synthetic ONNX model covering operators that openWakeWord
wake-word heads exported by newer PyTorch versions may use (opset 17:
LayerNormalization, MatMul + Add, Reshape, Unsqueeze / Squeeze with axes
inputs, Clip with min / max inputs, Softmax, ReduceMean, Gemm with transB),
and its ONNX Runtime output, for the interpreter test in src/nn.rs.

Usage: python gen_ops_model.py   (needs numpy, onnx, onnxruntime)
Input: x[0, i, j] = sin(0.37 * (i * 96 + j)), shape [1, 16, 96].
"""
import os

import numpy as np
import onnx
import onnxruntime as ort
from onnx import TensorProto, helper, numpy_helper

HERE = os.path.dirname(os.path.abspath(__file__))
rng = np.random.default_rng(1234)


def init(name, a):
    return numpy_helper.from_array(np.asarray(a), name)


inits = [
    init("w1", (rng.standard_normal((24, 1536)) * 0.05).astype(np.float32)),
    init("b1", (rng.standard_normal(24) * 0.1).astype(np.float32)),
    init("ln_s", (1 + rng.standard_normal(24) * 0.1).astype(np.float32)),
    init("ln_b", (rng.standard_normal(24) * 0.1).astype(np.float32)),
    init("w2", (rng.standard_normal((24, 16)) * 0.3).astype(np.float32)),
    init("b2", (rng.standard_normal(16) * 0.1).astype(np.float32)),
    init("shape", np.array([1, 4, -1], np.int64)),
    init("ax1", np.array([1], np.int64)),
    init("lo", np.array(-0.8, np.float32)),
    init("hi", np.array(0.9, np.float32)),
]
nodes = [
    helper.make_node("Flatten", ["x"], ["f"], axis=1),
    helper.make_node("Gemm", ["f", "w1", "b1"], ["g"], transB=1),
    helper.make_node("LayerNormalization", ["g", "ln_s", "ln_b"], ["ln"], axis=-1, epsilon=1e-5),
    helper.make_node("Relu", ["ln"], ["r"]),
    helper.make_node("MatMul", ["r", "w2"], ["m"]),
    helper.make_node("Add", ["m", "b2"], ["a"]),
    helper.make_node("Reshape", ["a", "shape"], ["rs"]),
    helper.make_node("Unsqueeze", ["rs", "ax1"], ["u"]),
    helper.make_node("Squeeze", ["u", "ax1"], ["sq"]),
    helper.make_node("Tanh", ["sq"], ["t"]),
    helper.make_node("Clip", ["t", "lo", "hi"], ["c"]),
    helper.make_node("Softmax", ["c"], ["sm"], axis=-1),
    helper.make_node("ReduceMean", ["sm"], ["rm"], axes=[2], keepdims=1),
    helper.make_node("Transpose", ["sm"], ["tr"], perm=[0, 2, 1]),
    helper.make_node("Sub", ["tr", "rm"], ["d"]),
    helper.make_node("Sigmoid", ["d"], ["y"]),
]
graph = helper.make_graph(
    nodes, "ops", [helper.make_tensor_value_info("x", TensorProto.FLOAT, [1, 16, 96])],
    [helper.make_tensor_value_info("y", TensorProto.FLOAT, [1, 4, 4])], inits)
model = helper.make_model(graph, opset_imports=[helper.make_opsetid("", 17)])
model.ir_version = 8
onnx.checker.check_model(model)
path = os.path.join(HERE, "ops_model.onnx")
onnx.save(model, path)

x = np.sin(0.37 * np.arange(16 * 96)).astype(np.float32).reshape(1, 16, 96)
(y,) = ort.InferenceSession(path, providers=["CPUExecutionProvider"]).run(None, {"x": x})
with open(os.path.join(HERE, "ops_model_ref.txt"), "w") as f:
    f.write(" ".join(str(d) for d in y.shape) + "\n")
    f.write(" ".join(f"{float(v):.9g}" for v in y.ravel()) + "\n")

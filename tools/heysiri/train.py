"""Train the three stages of the "Hey Siri" detector and write the weight
blob `crates/dsp/src/heysiri.bin`.

Usage: train.py <workdir> <heysiri_dump binary> <out.bin> [--all]

Features come from the Rust front end itself (`heysiri_dump`), so training
and run time see identical inputs.  The clips are split three ways by
speaker / voice / recording: train, validation (sets the thresholds) and
test (reported on; unseen Piper speakers and Polly voices, the "legacy-tts"
clips, a quarter of the other Tigre clips, the FLEURS test split).  `--all`
adds the test part to the training set for the shipped model.
"""
import hashlib, json, os, re, struct, subprocess, sys

import numpy as np
import torch
import torch.nn as nn
import torch.nn.functional as F

WORK, DUMP, OUT = sys.argv[1:4]
ALL = "--all" in sys.argv
NMEL, PRE, POST = 32, 40, (0, 12, 25)
W = PRE + 1 + POST[2]
HOP = 480
RANGE = 11.5
# thresholds: this share of the validation "hey siri" passes each stage ...
TARGET_RECALL = (0.99, 0.985, 0.985)
# ... unless more than this share of the other validation /s/ onsets would
MAX_PASS = (0.10, 0.05, 0.03)
RUN_GAP = 8   # onsets closer than this (frames) belong to one /s/


def split_of(m):
    """0 train, 1 validation, 2 test."""
    group = m["group"]
    h = lambda t: int(hashlib.md5(t.encode()).hexdigest(), 16)
    clip = re.sub(r"_[01]\.f32$", "", os.path.basename(m["raw"]))   # both copies of a recording together
    if group.startswith("tigre/legacy-tts") or group.endswith("/test"):
        return 2
    if group.startswith("tigre/polly"):
        return (0, 1, 2)[h(group) % 3]
    if group.startswith("tigre/"):
        return {0: 2, 1: 1}.get(h(clip) % 4, 0)
    if ":" in group:   # piper voice:speaker
        return {0: 2, 1: 1}.get(h(group) % 6, 0)
    return 1 if h(clip) % 5 == 0 else 0


def load():
    metas = [json.loads(l) for l in open(os.path.join(WORK, "manifest.jsonl"))]
    lst, dump = os.path.join(WORK, "list.txt"), os.path.join(WORK, "feats.bin")
    open(lst, "w").write("\n".join(m["raw"] for m in metas) + "\n")
    subprocess.run([DUMP, lst, dump], check=True)
    buf = np.memmap(dump, dtype=np.uint8, mode="r")
    off = 0
    X, rows = [], []   # rows: (clip, frame, y0, y1, y2); y: 1 pos, 0 neg, -1 ignore
    for ci, m in enumerate(metas):
        n = int(np.frombuffer(buf, dtype="<u4", count=1, offset=off)[0])
        off += 4
        frames = []
        for _ in range(n):
            frames.append(int(np.frombuffer(buf, dtype="<i4", count=1, offset=off)[0]))
            off += 4
            X.append(np.frombuffer(buf, dtype="<f4", count=W * NMEL, offset=off).reshape(W, NMEL))
            off += 4 * W * NMEL
        m["onsets"] = frames
        m["seconds"] = os.path.getsize(m["raw"]) / 4 / 48000
        span = m.get("span")
        # /s/ sounds inside the phrase: runs of onsets; the target is the
        # first onset of the last run ("hey siri" has one /s/) or, for the
        # "hey s-other" phrases, of the first run
        inside = [f for f in frames if span and span[0] + 2880 <= f * HOP < span[1]]
        runs = []
        for f in inside:
            if runs and f - runs[-1][-1] < RUN_GAP:
                runs[-1].append(f)
            else:
                runs.append([f])
        target = None
        if runs:
            target = (runs[-1] if m["kind"] == "pos" else runs[0])[0]
        m["target"] = target
        for f in frames:
            if span and span[0] <= f * HOP < span[1]:
                if f != target:
                    y = (-1, -1, -1)
                elif m["kind"] == "pos":
                    y = (1, 1, 1)
                else:
                    y = (-1, 0 if m["amb"] == 0 else -1, 0 if m["amb"] <= 1 else -1)
            else:
                y = (0, 0, 0)
            rows.append((ci, f) + y)
    assert off == len(buf)
    return metas, torch.tensor(np.stack(X)), np.array(rows)


def normalize(x):
    """(N, frames, mel) raw log-mel -> network input (N, mel, frames); as `heysiri::normalize`."""
    mx = x.reshape(len(x), -1).max(dim=1).values[:, None, None]
    return ((x - mx).clamp(min=-RANGE) / RANGE * 2 + 1).transpose(1, 2)


def augment(x):
    """Speaker / rate variation on raw (N, W, mel) windows: stretch the time
    axis around the onset frame and warp the mel axis."""
    n = len(x)
    rate = torch.exp((torch.rand(n, 1) - 0.5) * 0.44)
    t = (PRE + (torch.arange(W)[None, :] - PRE) * rate).clamp(0, W - 1)
    alpha = 1 + (torch.rand(n, 1) - 0.5) * 0.16
    f = (torch.arange(NMEL)[None, :] * alpha + (torch.rand(n, 1) - 0.5) * 1.4).clamp(0, NMEL - 1)
    grid = torch.stack([(f / (NMEL - 1) * 2 - 1)[:, None, :].expand(n, W, NMEL),
                        (t / (W - 1) * 2 - 1)[:, :, None].expand(n, W, NMEL)], dim=3)
    return F.grid_sample(x[:, None], grid, mode="bilinear", padding_mode="border", align_corners=True)[:, 0]


class Net(nn.Module):
    def __init__(self, frames):
        super().__init__()
        self.c1 = nn.Conv1d(NMEL, 24, 5, stride=2)
        self.c2 = nn.Conv1d(24, 32, 5, stride=2)
        t = ((frames - 5) // 2 + 1 - 5) // 2 + 1
        self.drop = nn.Dropout(0.25)
        self.f1 = nn.Linear(32 * t, 32)
        self.f2 = nn.Linear(32, 1)

    def forward(self, x):
        x = torch.relu(self.c1(x))
        x = torch.relu(self.c2(x))
        x = torch.relu(self.f1(self.drop(x.flatten(1))))
        return self.f2(x).squeeze(1)


def train(x, y, frames, seed):
    torch.manual_seed(seed)
    net = Net(frames)
    opt = torch.optim.AdamW(net.parameters(), 2e-3, weight_decay=1e-2)
    epochs = 30
    sched = torch.optim.lr_scheduler.OneCycleLR(opt, 4e-3, total_steps=epochs * (len(x) // 256 + 1))
    w = torch.where(y > 0.5, (1 - y.mean()) / y.mean(), torch.ones_like(y)).clamp(max=20.0)
    for _ in range(epochs):
        net.train()
        perm = torch.randperm(len(x))
        for i in range(0, len(x), 256):
            idx = perm[i:i + 256]
            xb = normalize(augment(x[idx])[:, :frames])
            # level jitter, spectral tilt, feature noise
            tilt = torch.linspace(-1, 1, NMEL)[None, :, None] * (torch.rand(len(idx), 1, 1) - 0.5) * 0.5
            xb = (xb + tilt + (torch.rand(len(idx), 1, 1) - 0.5) * 0.2 + torch.randn_like(xb) * 0.05).clamp(-1.2, 1.2)
            loss = F.binary_cross_entropy_with_logits(net(xb), y[idx], weight=w[idx])
            opt.zero_grad()
            loss.backward()
            opt.step()
            sched.step()
    net.eval()
    return net


def main():
    metas, X, rows = load()
    split = np.array([split_of(m) for m in metas])
    if ALL:
        split[split == 2] = 0
    part = split[rows[:, 0]]
    print(f"{len(metas)} clips, {len(rows)} onsets: train {(part == 0).sum()}, validation {(part == 1).sum()},"
          f" test {(part == 2).sum()}")
    nets, thr, logits = [], [], []
    for s in range(3):
        y = rows[:, 2 + s]
        use = torch.tensor((y >= 0) & (part == 0))
        frames = PRE + 1 + POST[s]
        net = train(X[use], torch.tensor(y, dtype=torch.float32)[use], frames, s)
        with torch.no_grad():
            z = torch.cat([net(normalize(X[i:i + 4096, :frames])) for i in range(0, len(X), 4096)]).numpy()
        val = part == 1
        t_recall = float(np.quantile(z[val & (y == 1)], 1 - TARGET_RECALL[s]))
        t_pass = float(np.quantile(z[val & (y == 0)], 1 - MAX_PASS[s]))
        t = max(t_recall, t_pass)
        nets.append(net); thr.append(t); logits.append(z)
        print(f"stage {s}: train pos {int((y[use.numpy()] == 1).sum())}, neg {int((y[use.numpy()] == 0).sum())};"
              f" threshold {t:.2f} (recall target {t_recall:.2f}, pass limit {t_pass:.2f})")
    passed = [logits[s] >= thr[s] for s in range(3)]

    def report(name, clips):
        sel = np.isin(rows[:, 0], clips)
        if not sel.any():
            return
        secs = sum(metas[c]["seconds"] for c in clips)
        y = rows[:, 2:5]
        pos = sel & (y[:, 0] == 1)
        n_pos_clips = sum(1 for c in clips if metas[c]["kind"] == "pos")
        line = f"{name}: {len(clips)} clips, {secs / 60:.1f} min"
        if n_pos_clips:
            found = int(pos.sum())
            full = int((pos & passed[0] & passed[1] & passed[2]).sum())
            line += (f" | hey siri: /s/ found {found}/{n_pos_clips}, stage0 {int((pos & passed[0]).sum())},"
                     f" all stages {full} ({100 * full / n_pos_clips:.1f}% of clips)")
        neg = sel & (y[:, 0] == 0)
        if neg.any():
            p0 = neg & passed[0]
            p1 = p0 & passed[1]
            p2 = p1 & passed[2]
            notch = (p0.sum() - p1.sum()) * 0.15 + (p1.sum() - p2.sum()) * 0.28 + p2.sum() * 0.48
            # plain sibilant mode: 0.48 s per onset, 0.25 s refractory
            sib = 0
            for c in clips:
                last = -1e9
                for f in metas[c]["onsets"]:
                    if f - last > 25:
                        last = f
                        sib += 1
            line += (f" | other /s/ onsets {int(neg.sum())}: stage0 {int(p0.sum())}, stage1 {int(p1.sum())},"
                     f" stage2 {int(p2.sum())}; notched {100 * notch / secs:.2f}% of the time"
                     f" (sibilant mode {100 * min(1.0, sib * 0.48 / secs):.0f}%)")
        hard = sel & (y[:, 0] == -1) & (y[:, 2] == 0)
        if hard.any():
            line += (f" | 'hey s-other' {int(hard.sum())}: stage0 {int((hard & passed[0]).sum())},"
                     f" full length {int((hard & passed[0] & passed[1] & passed[2]).sum())}")
        print(line)

    def family(m):
        g = m["group"]
        return g.split(":")[0] if ":" in g else "/".join(g.split("/")[:2]) if g.startswith("tigre/") else g

    for name, k in (("TRAIN", 0), ("VALIDATION", 1), ("TEST", 2)):
        if not (split == k).any():
            continue
        print(name)
        for g in sorted({family(m) for m in metas}):
            clips = [i for i, m in enumerate(metas) if family(m) == g and split[i] == k]
            if clips:
                report("  " + g, clips)
        kinds = {"all english 'hey siri' material": lambda m: m["kind"] != "neg",
                 "all other material": lambda m: m["kind"] == "neg"}
        for label, pick in kinds.items():
            report("  = " + label, [i for i, m in enumerate(metas) if pick(m) and split[i] == k])

    with open(OUT, "wb") as f:
        f.write(b"HSD1")
        for net, t in zip(nets, thr):
            f.write(struct.pack("<f", t))
            for p in (net.c1.weight, net.c1.bias, net.c2.weight, net.c2.bias,
                      net.f1.weight, net.f1.bias, net.f2.weight, net.f2.bias):
                a = p.detach().numpy().astype("<f4")
                f.write(struct.pack("<I", a.size))
                f.write(a.tobytes())
    print("wrote", OUT, os.path.getsize(OUT), "bytes")


if __name__ == "__main__":
    main()

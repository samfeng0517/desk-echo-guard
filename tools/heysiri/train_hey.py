"""Train the "hey" stage of the "Hey Siri" detector (the `hey` profile:
the notch opens while the "hey" is still being said) and write
`crates/dsp/src/heysiri_hey.bin`.

Usage: train_hey.py <workdir> <heysiri_dump binary> <out.bin> [--all]

Unlike the other stages (train.py), which look at the audio around an /s/
onset, this one is evaluated on every 10 ms frame: do the last 0.25 s end
60-120 ms into the vowel of a "hey" (that an /s/ will follow)?  Same data,
split and `--all` as train.py.
"""
import json, os, struct, subprocess, sys

import numpy as np
import torch
import torch.nn.functional as F

import train as T

WORK, DUMP, OUT = sys.argv[1:4]
ALL = "--all" in sys.argv
NMEL, HOP = T.NMEL, T.HOP
FRAMES = 25            # window: 0.25 s ending with the current frame
POS = (6, 12)          # positive frames: this far (frames) into the vowel
REFRACTORY = 30        # frames between two triggers
HOLD = 35              # frames the notch stays open waiting for the /s/
# threshold: at most this many triggers per minute of other validation speech
MAX_PER_MIN = 3.0
ZH_HEY = ("嘿", "黑")   # Mandarin phrases that do start with "hei"


def load():
    metas = [json.loads(l) for l in open(os.path.join(WORK, "manifest.jsonl"))]
    lst, dump = os.path.join(WORK, "list.txt"), os.path.join(WORK, "frames.bin")
    if not os.path.exists(dump):
        open(lst, "w").write("\n".join(m["raw"] for m in metas) + "\n")
        subprocess.run([DUMP, "--frames", lst, dump], check=True)
    buf = np.memmap(dump, dtype=np.uint8, mode="r")
    silence = np.full((FRAMES - 1, NMEL), np.log(1e-10), dtype=np.float32)
    parts, off, base = [], 0, 0
    label = []          # per frame: 1 positive, 0 negative, -1 ignore
    clip_of = []
    for ci, m in enumerate(metas):
        n = int(np.frombuffer(buf, dtype="<u4", count=1, offset=off)[0]); off += 4
        onsets = np.frombuffer(buf, dtype="<i4", count=n, offset=off).tolist(); off += 4 * n
        nf = int(np.frombuffer(buf, dtype="<u4", count=1, offset=off)[0]); off += 4
        x = np.frombuffer(buf, dtype="<f4", count=nf * NMEL, offset=off).reshape(nf, NMEL); off += 4 * nf * NMEL
        y = np.zeros(nf, dtype=np.int8)
        m["frames"], m["hey"] = nf, None
        span = m.get("span")
        is_hey = m["kind"] == "pos" or (m["kind"] == "hard" and (
            m["text"].lower().startswith("hey") or m["text"].startswith(ZH_HEY)))
        if span:
            a, b = span[0] // HOP, min(nf, span[1] // HOP + 1)
            inside = [f for f in onsets if span[0] + 2880 <= f * HOP < span[1]]
            runs = []
            for f in inside:
                if runs and f - runs[-1][-1] < T.RUN_GAP:
                    runs[-1].append(f)
                else:
                    runs.append([f])
            if is_hey and runs:
                fs = (runs[-1] if m["kind"] == "pos" else runs[0])[0]   # the /s/ after the "hey"
                loud = x[a:fs].max(axis=1)
                # vowel onset: first frame within 10 dB of the loudest one before the /s/
                v0 = a + int(np.argmax(loud > loud.max() - 2.3))
                y[max(0, a - 5):min(nf, fs + 50)] = -1
                lo, hi = v0 + POS[0], min(v0 + POS[1], fs - 1)
                if hi >= lo:
                    y[lo:hi + 1] = 1
                    m["hey"] = (v0, fs)
            elif is_hey:
                y[a:b] = -1      # no /s/ found: unusable
            # other "hard" phrases (沒事, 給誰, they say ...): plain negatives
        parts += [silence, x]
        label += [np.full(FRAMES - 1, -1, dtype=np.int8), y]
        clip_of += [np.full(FRAMES - 1 + nf, ci, dtype=np.int32)]
        m["base"] = base + FRAMES - 1      # index of the clip's frame 0
        base += FRAMES - 1 + nf
    assert off == len(buf)
    return metas, torch.tensor(np.concatenate(parts)), np.concatenate(label), np.concatenate(clip_of)


def windows(X, idx):
    """Raw (n, FRAMES, mel) windows ending at the frames `idx`."""
    return X[torch.as_tensor(idx)[:, None] + torch.arange(-FRAMES + 1, 1)[None, :]]


def augment(x):
    n = len(x)
    rate = torch.exp((torch.rand(n, 1) - 0.5) * 0.44)
    t = (FRAMES - 1 + (torch.arange(FRAMES)[None, :] - (FRAMES - 1)) * rate).clamp(0, FRAMES - 1)
    alpha = 1 + (torch.rand(n, 1) - 0.5) * 0.16
    f = (torch.arange(NMEL)[None, :] * alpha + (torch.rand(n, 1) - 0.5) * 1.4).clamp(0, NMEL - 1)
    grid = torch.stack([(f / (NMEL - 1) * 2 - 1)[:, None, :].expand(n, FRAMES, NMEL),
                        (t / (FRAMES - 1) * 2 - 1)[:, :, None].expand(n, FRAMES, NMEL)], dim=3)
    return F.grid_sample(x[:, None], grid, mode="bilinear", padding_mode="border", align_corners=True)[:, 0]


def scores(net, X, idx):
    out = []
    with torch.no_grad():
        for i in range(0, len(idx), 16384):
            out.append(net(T.normalize(windows(X, idx[i:i + 16384]))))
    return torch.cat(out).numpy()


def train(X, pos, neg):
    torch.manual_seed(0)
    rng = np.random.default_rng(0)
    net = T.Net(FRAMES)
    epochs, per = 24, 4
    steps = epochs * ((len(pos) * (1 + per)) // 256 + 1)
    opt = torch.optim.AdamW(net.parameters(), 2e-3, weight_decay=1e-2)
    sched = torch.optim.lr_scheduler.OneCycleLR(opt, 4e-3, total_steps=steps)
    hard = np.empty(0, dtype=np.int64)
    for ep in range(epochs):
        if ep in (6, 12, 18):     # hard negative mining over a third of the negatives
            net.eval()
            cand = neg[rng.random(len(neg)) < 0.34]
            z = scores(net, X, cand)
            hard = np.unique(np.concatenate([hard, cand[np.argsort(z)[-len(pos) * 3:]]]))
            print(f"  epoch {ep}: {len(hard)} hard negatives, top score {z.max():.2f}", flush=True)
        net.train()
        n_neg = len(pos) * per
        if len(hard):
            nidx = np.concatenate([rng.choice(hard, n_neg // 2), rng.choice(neg, n_neg - n_neg // 2)])
        else:
            nidx = rng.choice(neg, n_neg)
        idx = np.concatenate([pos, nidx])
        y = torch.cat([torch.ones(len(pos)), torch.zeros(len(nidx))])
        perm = rng.permutation(len(idx))
        for i in range(0, len(idx), 256):
            b = perm[i:i + 256]
            xb = T.normalize(augment(windows(X, idx[b])))
            tilt = torch.linspace(-1, 1, NMEL)[None, :, None] * (torch.rand(len(b), 1, 1) - 0.5) * 0.5
            xb = (xb + tilt + (torch.rand(len(b), 1, 1) - 0.5) * 0.2 + torch.randn_like(xb) * 0.05).clamp(-1.2, 1.2)
            w = torch.where(y[b] > 0.5, float(per), 1.0)
            loss = F.binary_cross_entropy_with_logits(net(xb), y[b], weight=w)
            opt.zero_grad(); loss.backward(); opt.step(); sched.step()
    net.eval()
    return net


def triggers(z, thr):
    """Frames at which the detector fires (refractory applied)."""
    out, last = [], -10 ** 9
    for f in np.nonzero(z >= thr)[0]:
        if f - last >= REFRACTORY:
            out.append(f); last = f
    return out


def main():
    metas, X, label, clip_of = load()
    split = np.array([T.split_of(m) for m in metas])
    if ALL:
        split[split == 2] = 0
    part = split[clip_of]
    all_idx = np.arange(len(label))
    pos = all_idx[(label == 1) & (part == 0)]
    neg = all_idx[(label == 0) & (part == 0)]
    print(f"{len(metas)} clips, {len(label)} frames; train: {len(pos)} positive, {len(neg)} negative frames", flush=True)
    net = train(X, pos, neg)

    # per clip scores, for the trigger simulation
    z_clip = {}
    for ci, m in enumerate(metas):
        if split[ci] == 0 and not (ALL and m["hey"]):
            continue
        z_clip[ci] = scores(net, X, np.arange(m["base"], m["base"] + m["frames"]))

    def stats(clips, thr):
        hey = [c for c in clips if metas[c]["hey"]]
        other = [c for c in clips if metas[c]["kind"] == "neg"]
        hit, leads = 0, []
        for c in hey:
            v0, fs = metas[c]["hey"]
            # in time: fired before the /s/ and the region (HOLD) is still open there
            t = [f for f in triggers(z_clip[c], thr) if f <= fs <= f + HOLD]
            if t:
                hit += 1; leads.append((fs - t[0]) / 100)
        n_trig = sum(len(triggers(z_clip[c], thr)) for c in other)
        mins = sum(metas[c]["frames"] for c in other) / 6000
        return hit, len(hey), (np.median(leads) if leads else 0), n_trig, mins

    val = [c for c in z_clip if split[c] == 1]
    lo, hi = -8.0, 12.0     # bisect the threshold on the validation trigger rate
    for _ in range(30):
        mid = (lo + hi) / 2
        _, _, _, n, mins = stats(val, mid)
        lo, hi = (mid, hi) if n / mins > MAX_PER_MIN else (lo, mid)
    thr = hi
    print(f"threshold {thr:.2f} ({MAX_PER_MIN}/min on the other validation speech)")

    def family(m):
        g = m["group"]
        return g.split(":")[0] if ":" in g else "/".join(g.split("/")[:2]) if g.startswith("tigre/") else g

    for name, k in (("VALIDATION", 1), ("TEST", 2)):
        clips = [c for c in z_clip if split[c] == k]
        if not clips:
            continue
        print(name)
        groups = sorted({family(metas[c]) for c in clips}) + ["= all"]
        for g in groups:
            sel = clips if g == "= all" else [c for c in clips if family(metas[c]) == g]
            hit, n, lead, trig, mins = stats(sel, thr)
            line = f"  {g}: {len(sel)} clips"
            if n:
                line += f" | 'hey' + /s/ phrases caught in time {hit}/{n} ({100 * hit / n:.1f}%), median {lead:.2f} s before the /s/"
            if mins > 0:
                line += f" | other speech {mins:.0f} min: {trig / mins:.1f} triggers/min"
            print(line)
        if g == "= all":
            for alt in (2.0, 4.0, 10.0, 20.0):   # the trade-off around the chosen threshold
                lo2, hi2 = -8.0, 12.0
                for _ in range(30):
                    mid = (lo2 + hi2) / 2
                    lo2, hi2 = (mid, hi2) if stats(val, mid)[3] / stats(val, mid)[4] > alt else (lo2, mid)
                hit, n, lead, trig, mins = stats(clips, hi2)
                print(f"    at {alt:.0f}/min on validation: caught {100 * hit / max(n, 1):.1f}%, {trig / max(mins, 1e-9):.1f}/min here")

    with open(OUT, "wb") as f:
        f.write(b"HSH1")
        f.write(struct.pack("<f", thr))
        for p in (net.c1.weight, net.c1.bias, net.c2.weight, net.c2.bias,
                  net.f1.weight, net.f1.bias, net.f2.weight, net.f2.bias):
            a = p.detach().numpy().astype("<f4")
            f.write(struct.pack("<I", a.size))
            f.write(a.tobytes())
    print("wrote", OUT, os.path.getsize(OUT), "bytes")


if __name__ == "__main__":
    main()

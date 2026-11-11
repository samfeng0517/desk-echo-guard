"""Build the training / test audio for the "Hey Siri" detector.

Usage: gen_data.py <workdir> [n_workers]

Expects under <workdir>/data (see README.md):
  piper/...        Piper voices (rhasspy/piper-voices)
  tigre/train      TigreGotico/synthetic-wakeword-hey_siri
  fleurs/data/...  google/fleurs audio (cmn_hans_cn, yue_hant_hk, en_us)
Writes 48 kHz mono f32le clips to <workdir>/raw and <workdir>/manifest.jsonl.
"""
import glob, hashlib, json, os, random, subprocess, sys
from multiprocessing import Pool

import numpy as np
import soundfile as sf
from scipy.signal import resample_poly

FS = 48000
WORK = sys.argv[1]
DATA = os.path.join(WORK, "data")
RAW = os.path.join(WORK, "raw")

HS = ["Hey Siri", "Hey Siri.", "Hey, Siri", "Hey Siri!", "Hey Siri?", "hey Siri,"]
# "hey" + another s-word.  amb 0: differs right after the /s/; 1: starts like
# "si-", differs later; 2: sounds like "hey siri" to the end of the window.
HARD = [
    ("Hey Sam", 0), ("Hey Sarah", 0), ("Hey sorry", 0), ("Hey Susan", 0), ("Hey so", 0),
    ("Hey Sunday", 0), ("Hey Sophie", 0), ("Hey Sean", 0), ("Hey sugar", 0), ("Hey Sheila", 0),
    ("They say", 0), ("They said", 0), ("Hey Saturday", 0), ("Hey Sally", 0), ("Hey send it", 0),
    ("They saw", 0), ("Hey summer", 0), ("Hey Simon", 0), ("Hey Seth", 0),
    ("Hey Cindy", 1), ("Hey sister", 1), ("Hey silly", 1), ("Hey Sid", 1), ("Hey city", 1),
    ("Hey see", 1), ("Hey Seattle", 1), ("Hey sit", 1), ("Hey six", 1), ("Hey Sierra", 1),
    ("Hey Sylvia", 1), ("Hey singer", 1),
    ("Hey serious", 2), ("Hey Syria", 2), ("Hey cereal", 2), ("Hey sir", 2),
]
ZH_HARD = ["黑色", "嘿，四個", "嘿，三明治", "嘿，撒嬌", "黑絲", "飛速", "嘿，蘇打", "嘿，森林",
           "嘿，喜歡", "嘿，小明", "沒事", "誰說", "給誰", "被殺", "嘿，先生", "嘿，松鼠",
           "黑森林", "嘿，司機", "嘿，四月", "非洲", "嘿，傘", "嘿，想你", "累死", "美色"]
EN = [
    "What's the weather like today?", "Set a timer for six minutes.", "Play some music in the kitchen.",
    "So I said yes, of course.", "She sells sea shells by the sea shore.", "This is such a strange situation.",
    "Send a message to my sister.", "Yesterday I saw seven small birds.", "Call me as soon as possible.",
    "I was just saying the same thing.", "Could you turn on the lights, please?", "That sounds serious to me.",
    "Remind me to buy cheese and juice.", "How many steps did I take this week?", "It's a nice sunny Saturday.",
    "Let's see what happens next.", "Stop, just listen for a second.", "My phone keeps answering by itself.",
    "I always say that and it never listens.", "Okay, so where were we?", "Sorry, I missed what you said.",
    "Then I asked it to stop the song.", "Sometimes it wakes up when nobody asked.", "Please skip this one.",
    "The series starts at six thirty.", "He said hey and she said hi.", "Hey, how are you doing?",
    "Hey there, is this seat taken?", "Say something, hey, say something.", "They sent us seventeen samples.",
]
ZH = [
    "今天天氣怎麼樣，要不要出去走走？", "四十四隻石獅子，是死的還是活的。", "我昨天去超市買了三瓶醬油和一些蔬菜。",
    "這個意思是說，我們下星期三再開會。", "謝謝你，真的非常感謝。", "他說他沒事，只是有點累。", "黑色的車子停在學校前面。",
    "請問現在幾點了，我想去洗手間。", "所以說，事情就是這樣發生的。", "先生，請問這個座位有人坐嗎？",
    "我喜歡吃西瓜、草莓，還有橘子。", "小心一點，下雨天路很滑。", "這首歌實在太好聽了，再放一次。",
    "三十三個學生參加了這次的考試。", "誰說的，我才沒有這樣想。", "給誰都可以，隨便你決定。", "司機先生，麻煩前面右轉。",
    "手機自己響起來，嚇死我了。", "這次的測試成績出來了嗎？", "我們去吃壽司還是吃火鍋？",
    "沒事沒事，你先忙你的。", "他飛速地跑過去，一下子就不見了。", "新的系統設定好像有一點問題。",
    "星期四晚上七點在車站集合。", "西瑞是誰，我不認識。", "就是說啊，真的太誇張了。",
    "森林裡住著四隻小松鼠。", "請把聲音關小一點，謝謝。", "這是在做什麼，怎麼這麼吵？",
    "我想先休息一下，晚一點再說。", "差不多七八成的人都知道這件事。", "自己的事情自己做。",
]
EN_MULTI = ["en/en_US/libritts_r/medium/en_US-libritts_r-medium", "en/en_GB/vctk/medium/en_GB-vctk-medium",
            "en/en_US/l2arctic/medium/en_US-l2arctic-medium", "en/en_US/arctic/medium/en_US-arctic-medium"]
EN_SINGLE = ["en/en_US/lessac/medium/en_US-lessac-medium", "en/en_US/ryan/medium/en_US-ryan-medium",
             "en/en_US/amy/medium/en_US-amy-medium", "en/en_US/joe/medium/en_US-joe-medium",
             "en/en_US/hfc_female/medium/en_US-hfc_female-medium", "en/en_US/hfc_male/medium/en_US-hfc_male-medium",
             "en/en_GB/alan/medium/en_GB-alan-medium", "en/en_GB/cori/medium/en_GB-cori-medium"]
ZH_VOICES = ["zh/zh_CN/huayan/medium/zh_CN-huayan-medium", "zh/zh_CN/chaowen/medium/zh_CN-chaowen-medium",
             "zh/zh_CN/xiao_ya/medium/zh_CN-xiao_ya-medium"]

_voices = {}


def tts(voice, text, speaker, rng):
    from piper import PiperVoice, SynthesisConfig
    if voice not in _voices:
        _voices[voice] = PiperVoice.load(os.path.join(DATA, "piper", voice + ".onnx"))
    v = _voices[voice]
    cfg = SynthesisConfig(speaker_id=speaker, length_scale=rng.uniform(0.8, 1.3),
                          noise_scale=rng.uniform(0.4, 0.9), noise_w_scale=rng.uniform(0.6, 1.0))
    a = np.concatenate([c.audio_float_array for c in v.synthesize(text, syn_config=cfg)])
    return to48k(a, v.config.sample_rate)


def to48k(a, sr):
    a = np.asarray(a, dtype=np.float64)
    if a.ndim > 1:
        a = a.mean(axis=1)
    if sr != FS:
        g = np.gcd(FS, sr)
        a = resample_poly(a, FS // g, sr // g)
    return a.astype(np.float32)


def trim(a, thr_db=-40.0):
    e = np.abs(a)
    lim = e.max() * 10 ** (thr_db / 20)
    idx = np.nonzero(e > lim)[0]
    return a if len(idx) == 0 else a[max(0, idx[0] - 240):idx[-1] + 240]


def opus(a, rng):
    """Round trip through Opus, like a voice call."""
    p = subprocess.run(
        ["ffmpeg", "-v", "error", "-f", "f32le", "-ar", str(FS), "-ac", "1", "-i", "-", "-c:a", "libopus",
         "-b:a", f"{rng.choice([12, 16, 24, 32, 48, 64])}k", "-f", "ogg", "-"],
        input=a.astype(np.float32).tobytes(), capture_output=True, check=True)
    q = subprocess.run(["ffmpeg", "-v", "error", "-i", "-", "-f", "f32le", "-ar", str(FS), "-ac", "1", "-"],
                       input=p.stdout, capture_output=True, check=True)
    b = np.frombuffer(q.stdout, dtype=np.float32)
    out = np.zeros(len(a), dtype=np.float32)
    n = min(len(a), len(b))
    out[:n] = b[:n]
    return out


def augment(a, rng, nrng):
    a = a / (np.abs(a).max() + 1e-9) * 10 ** (rng.uniform(-30, -3) / 20)
    if rng.random() < 0.5:   # background noise, white or pink-ish
        n = nrng.standard_normal(len(a)).astype(np.float32)
        if rng.random() < 0.5:
            n = np.cumsum(n) * 0.05
            n -= np.convolve(n, np.ones(4800) / 4800, "same")
        snr = rng.uniform(15, 45)
        n *= np.sqrt(np.mean(a ** 2) / (np.mean(n ** 2) + 1e-12)) * 10 ** (-snr / 20)
        a = a + n.astype(np.float32)
    if rng.random() < 0.2:   # narrow-band source
        a = resample_poly(resample_poly(a, 1, 3), 3, 1)[:len(a)].astype(np.float32)
    if rng.random() < 0.45:
        a = opus(a, rng)
    return a.astype(np.float32)


def save(name, a, meta):
    path = os.path.join(RAW, name + ".f32")
    a = np.concatenate([a, np.zeros(FS // 2, dtype=np.float32)])
    a.astype("<f4").tofile(path)
    meta["raw"] = path
    return meta


def seed_of(*key):
    return int(hashlib.md5(repr(key).encode()).hexdigest()[:8], 16)


def phrase_job(job):
    """One clip: [sentence] gap PHRASE gap [sentence]; the phrase span is kept."""
    kind, voice, speaker, k, phrase, amb, ctx_voice = job
    rng = random.Random(seed_of(*job[:5]))
    nrng = np.random.default_rng(seed_of(*job[:5]))
    core = trim(tts(voice, phrase, speaker, rng))
    if rng.random() < 0.5:   # pitch / speed variation
        r = rng.uniform(0.9, 1.12)
        core = resample_poly(core, 100, int(round(100 * r))).astype(np.float32)
    sentences = ZH if "zh_CN" in ctx_voice else EN
    parts, span = [], None
    pos = 0
    if rng.random() < 0.45:
        pre = trim(tts(ctx_voice, rng.choice(sentences), speaker if ctx_voice == voice else None, rng))
        gap = np.zeros(int(rng.choice([0.03, 0.08, 0.15, 0.3, 0.6]) * FS), dtype=np.float32)
        parts += [pre, gap]
        pos = len(pre) + len(gap)
    else:
        lead = np.zeros(int(rng.uniform(0.05, 0.5) * FS), dtype=np.float32)
        parts.append(lead)
        pos = len(lead)
    parts.append(core)
    span = [pos, pos + len(core)]
    if rng.random() < 0.6:
        gap = np.zeros(int(rng.choice([0.0, 0.03, 0.1, 0.25, 0.5]) * FS), dtype=np.float32)
        parts += [gap, trim(tts(ctx_voice, rng.choice(sentences), speaker if ctx_voice == voice else None, rng))]
    a = augment(np.concatenate(parts), rng, nrng)
    name = f"{kind}_{os.path.basename(voice)}_{speaker}_{k}"
    return save(name, a, {"kind": kind, "span": span, "amb": amb, "group": f"{os.path.basename(voice)}:{speaker}",
                          "text": phrase})


def sentence_job(job):
    voice, k, text = job
    rng = random.Random(seed_of(*job))
    nrng = np.random.default_rng(seed_of(*job))
    a = augment(trim(tts(voice, text, None, rng)), rng, nrng)
    return save(f"neg_{os.path.basename(voice)}_{k}", a, {"kind": "neg", "group": os.path.basename(voice)})


def file_job(job):
    kind, path, aug, group = job
    rng = random.Random(seed_of(path, aug))
    nrng = np.random.default_rng(seed_of(path, aug))
    name = f"{kind}_{'-'.join(group.split('/')[:2])}_{os.path.basename(path)[:-4]}_{aug}"
    meta = {"kind": kind, "group": group}
    lead = int(rng.uniform(0.05, 0.4) * FS) if kind == "pos" else 0
    out = os.path.join(RAW, name + ".f32")
    if os.path.exists(out):   # resume an interrupted run
        n = os.path.getsize(out) // 4 - FS // 2
        if kind == "pos":
            meta["span"] = [lead, n]
        meta["raw"] = out
        return meta
    a, sr = sf.read(path, dtype="float32")
    a = np.concatenate([np.zeros(lead, dtype=np.float32), to48k(a, sr)])
    if kind == "pos":
        meta["span"] = [lead, len(a)]
    if aug:
        a = augment(a, rng, nrng)
    return save(name, a, meta)


def run(job):
    return job[0](job[1])


def main():
    os.makedirs(RAW, exist_ok=True)
    jobs = []
    spk = {"libritts_r": 904, "vctk": 109, "l2arctic": 24, "arctic": 18}
    reps = {"libritts_r": 3, "vctk": 8, "l2arctic": 30, "arctic": 20}
    rng = random.Random(1)
    for voice in EN_MULTI:
        key = next(k for k in spk if k + "/" in voice)
        for s in range(spk[key]):
            for k in range(reps[key]):
                ctx = rng.choice(ZH_VOICES) if rng.random() < 0.3 else voice
                jobs.append((phrase_job, ("pos", voice, s, k, rng.choice(HS), 0, ctx)))
            for k in range(max(1, reps[key] * 2 // 3)):
                p, amb = rng.choice(HARD)
                jobs.append((phrase_job, ("hard", voice, s, k, p, amb, voice)))
    for voice in EN_SINGLE:
        for k in range(60):
            ctx = rng.choice(ZH_VOICES) if rng.random() < 0.3 else voice
            jobs.append((phrase_job, ("pos", voice, None, k, rng.choice(HS), 0, ctx)))
        for k in range(40):
            p, amb = rng.choice(HARD)
            jobs.append((phrase_job, ("hard", voice, None, k, p, amb, voice)))
    for voice in ZH_VOICES:
        for k in range(6):
            for i, text in enumerate(ZH):
                jobs.append((sentence_job, (voice, f"{i}_{k}", text)))
        for k in range(12):
            for p in ZH_HARD:
                jobs.append((phrase_job, ("hard", voice, None, f"{k}_{ZH_HARD.index(p)}", p, 0, voice)))
    voice_of = {}
    for line in open(os.path.join(DATA, "tigre/train/metadata.jsonl")):
        d = json.loads(line)
        voice_of[d["file_name"]] = d["voice"] or d["speaker_id"] or "unknown"
    for f in sorted(glob.glob(os.path.join(DATA, "tigre/train/*/*.wav"))):
        origin = f.split("/")[-2]
        voice = voice_of.get(f"{origin}/{os.path.basename(f)}", "unknown")
        for aug in (0, 1):
            jobs.append((file_job, ("pos", f, aug, f"tigre/{origin}/{voice}")))
    for lang in ("cmn_hans_cn", "yue_hant_hk", "en_us"):
        for f in sorted(glob.glob(os.path.join(DATA, f"fleurs/data/{lang}/audio/*/*.wav"))):
            split = f.split("/")[-2]
            for aug in (0, 1):
                jobs.append((file_job, ("neg", f, aug, f"fleurs/{lang}/{split}")))
    print(len(jobs), "clips", flush=True)
    # group jobs by voice so each worker loads few models
    jobs.sort(key=lambda j: str(j[1][1]) if j[0] is not file_job else "")
    with Pool(int(sys.argv[2]) if len(sys.argv) > 2 else 4) as pool, \
            open(os.path.join(WORK, "manifest.jsonl"), "w") as out:
        for i, meta in enumerate(pool.imap(run, jobs, chunksize=64)):
            out.write(json.dumps(meta, ensure_ascii=False) + "\n")
            if i % 2000 == 0:
                print(i, flush=True)


if __name__ == "__main__":
    main()

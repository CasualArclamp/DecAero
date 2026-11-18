# made by CasualArclamp
"""Which bits of the 9.6 kbit/s voice frame are which predictor coefficient?

Judged without ears. The reflection coefficients of real speech have a
pattern of their own: how each moves with the others, and with itself and
the others one frame on. Those patterns, taken as rank correlations, do not
depend on how a coefficient was quantised, only on which coefficient it is
and which way up. So: measure them on real speech, measure them on the
fields of the voice frames, and find the assignment of fields to
coefficients (and signs) that makes the two agree best.

    python scripts/c21_match.py FRAMES.npy REF_WAV_GLOB OUT.json [MASK.npy] [FIELDS]

MASK.npy marks the frames to use (true/false, one a frame); without it all
but the idle frame are used. FIELDS is a list like "2:6,8:4,12:6t" (first
bit:width, t for two's complement) in place of the fields below.

FRAMES.npy: frames x 192 bits, in time order. The reference is 8 kHz mono
speech (16-bit WAVs).
"""
import glob
import json
import sys
import wave

import numpy as np

FIELDS = [(2, 6), (8, 4), (12, 4), (16, 2), (18, 5), (23, 3), (26, 4), (30, 4), (34, 2), (36, 3)]
# Field 12-15 wraps round (its histogram is a U): two's complement.
TWOS = {(12, 4)}
ORDER = 10


def reflection(frame):
    """Reflection coefficients of one windowed frame (autocorrelation method)."""
    r = np.array([frame[: len(frame) - k] @ frame[k:] for k in range(ORDER + 1)])
    if r[0] <= 0:
        return None
    a = np.zeros(ORDER + 1)
    a[0] = 1.0
    err = r[0]
    ks = []
    for m in range(1, ORDER + 1):
        k = -(r[m] + a[1:m] @ r[m - 1:0:-1]) / err
        ks.append(k)
        a[1:m + 1] = a[1:m + 1] + k * a[m - 1::-1][:m]
        err *= 1 - k * k
        if err <= 0:
            return None
    return ks


def reference(pattern):
    """Coefficient tracks of the reference speech: a list of runs of frames."""
    window = np.hamming(256)
    runs = []
    for path in sorted(glob.glob(pattern)):
        with wave.open(path) as w:
            x = np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16).astype(float)
        if len(x) < 4000:
            continue
        frames = [x[i:i + 256] for i in range(0, len(x) - 256, 160)]
        energy = np.array([np.mean(f * f) for f in frames])
        floor = np.percentile(energy[energy > 0], 30) if (energy > 0).any() else 1.0
        run = []
        for f, e in zip(frames, energy):
            ks = reflection(f * window) if e > floor else None
            if ks is None:
                if len(run) > 5:
                    runs.append(np.array(run))
                run = []
            else:
                run.append(ks)
        if len(run) > 5:
            runs.append(np.array(run))
    return runs


def fields(frames, mask=None):
    """The fields' values, as runs of consecutive live frames."""
    as_bytes = [bytes(f) for f in frames]
    idle = max(set(as_bytes), key=as_bytes.count)
    if mask is None:
        mask = [b != idle for b in as_bytes]
    runs, run = [], []
    for f, use in zip(frames, mask):
        if not use:
            if len(run) > 5:
                runs.append(np.array(run, dtype=float))
            run = []
            continue
        row = []
        for start, width in FIELDS:
            v = sum(int(bit) << i for i, bit in enumerate(f[start:start + width]))
            if (start, width) in TWOS and v >= 1 << (width - 1):
                v -= 1 << width
            row.append(v)
        run.append(row)
    if len(run) > 5:
        runs.append(np.array(run, dtype=float))
    return runs


def ranks(x):
    """Each column as ranks from -1 to 1 (ties share a rank)."""
    out = np.zeros_like(x)
    for j in range(x.shape[1]):
        order = np.argsort(x[:, j], kind="stable")
        r = np.empty(len(x))
        r[order] = np.arange(len(x))
        # Tied values get the mean of their ranks.
        for v in np.unique(x[:, j]):
            same = x[:, j] == v
            r[same] = r[same].mean()
        out[:, j] = r / (len(x) - 1) * 2 - 1
    return out


def patterns(runs):
    """Rank correlations between the columns, now and one and two frames on."""
    everything = np.vstack(runs)
    ranked = ranks(everything)
    pieces, at = [], 0
    for r in runs:
        pieces.append(ranked[at:at + len(r)])
        at += len(r)
    out = []
    for lag in (0, 1, 2):
        a = np.vstack([p[: len(p) - lag] for p in pieces if len(p) > lag])
        b = np.vstack([p[lag:] for p in pieces if len(p) > lag])
        a = a - a.mean(0)
        b = b - b.mean(0)
        out.append((a.T @ b) / np.sqrt(np.outer((a * a).sum(0), (b * b).sum(0))))
    return out


def cost(perm, signs, ref, got):
    total = 0.0
    for r, g in zip(ref, got):
        want = r[np.ix_(perm, perm)] * np.outer(signs, signs)
        total += ((want - g) ** 2).sum()
    return total


def search(ref, got, rng, tries=400):
    """Field i is coefficient perm[i], the way up signs[i]."""
    n = len(got[0])
    found = {}
    for _ in range(tries):
        perm = rng.permutation(n)
        signs = rng.choice([-1.0, 1.0], n)
        best = cost(perm, signs, ref, got)
        improved = True
        while improved:
            improved = False
            for i in range(n):
                signs[i] = -signs[i]
                c = cost(perm, signs, ref, got)
                if c < best - 1e-12:
                    best, improved = c, True
                else:
                    signs[i] = -signs[i]
            for i in range(n):
                for j in range(i + 1, n):
                    perm[i], perm[j] = perm[j], perm[i]
                    for flip in ((1, 1), (-1, 1), (1, -1), (-1, -1)):
                        signs[i] *= flip[0]
                        signs[j] *= flip[1]
                        c = cost(perm, signs, ref, got)
                        if c < best - 1e-12:
                            best, improved = c, True
                            break
                        signs[i] *= flip[0]
                        signs[j] *= flip[1]
                    else:
                        perm[i], perm[j] = perm[j], perm[i]
        # The whole set upside down in pairs gives the same patterns only
        # when every sign turns: keep one of the two.
        key = (tuple(int(p) for p in perm), tuple(int(s) for s in signs))
        found[key] = best
    return sorted(found.items(), key=lambda kv: kv[1])


def main():
    frames = np.load(sys.argv[1]).astype(np.uint8)
    ref_runs = reference(sys.argv[2])
    mask = np.load(sys.argv[4]) if len(sys.argv) > 4 and sys.argv[4] != "-" else None
    if len(sys.argv) > 5:
        global FIELDS, TWOS
        FIELDS, TWOS = [], set()
        for item in sys.argv[5].split(","):
            start, width = item.rstrip("t").split(":")
            FIELDS.append((int(start), int(width)))
            if item.endswith("t"):
                TWOS.add((int(start), int(width)))
    got_runs = fields(frames, mask)
    print("reference frames:", sum(len(r) for r in ref_runs), " voice frames:", sum(len(r) for r in got_runs))
    ref, got = patterns(ref_runs), patterns(got_runs)
    np.set_printoptions(precision=2, suppress=True, linewidth=200)
    print("reference, a frame on (diagonal: how steady each coefficient is):", np.diag(ref[1]))
    print("fields, a frame on:                                               ", np.diag(got[1]))
    rng = np.random.default_rng(0)
    results = search(ref, got, rng)
    n = len(FIELDS)
    chance = np.mean([cost(rng.permutation(10)[:n], rng.choice([-1.0, 1.0], n), ref, got) for _ in range(2000)])
    print(f"cost by chance about {chance:.2f}")
    for (perm, signs), c in results[:8]:
        text = "  ".join(f"{FIELDS[i][0]}-{FIELDS[i][0] + FIELDS[i][1] - 1}:{'+' if signs[i] > 0 else '-'}k{perm[i] + 1}" for i in range(len(FIELDS)))
        print(f"{c:6.3f}  {text}")
    best_perm, best_signs = results[0][0]
    with open(sys.argv[3], "w") as out:
        json.dump({"fields": FIELDS, "coefficient": [p + 1 for p in best_perm], "sign": list(best_signs),
                   "cost": results[0][1], "chance": chance,
                   "runner_up_cost": results[1][1] if len(results) > 1 else None}, out, indent=1)


if __name__ == "__main__":
    main()

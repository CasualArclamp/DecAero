# made by CasualArclamp
"""Trial recordings from the 21000 bit/s channel's voice frames.

The 9.6 kbit/s codec's quantisers are not known. This makes sound from
guesses at the predictor alone (the first 40 bits of each 192-bit frame,
taken as ten reflection coefficients), driven by hiss in place of the real
pulses: if a guess is right the result is whispered speech, if wrong it is
noise with no words in it. One WAV a guess, and a page to play them from.

    python scripts/c21_trials.py FRAMES.npy OUT_DIR [SECONDS]

FRAMES.npy: frames x 192 bits (0/1), in time order. SECONDS: the length of
the excerpt used (the liveliest stretch), 12 if not given.
"""
import itertools
import sys
import wave

import numpy as np
from numba import njit

# The ten fields the bits' statistics suggest (docs/DESIGN.md), low bit
# first: (first bit, width). Bits 0, 1 and 39 are left out.
A, B, C, D, E, F, G, H, I, J = (
    (2, 6), (8, 4), (12, 4), (16, 2), (18, 5), (23, 3), (26, 4), (30, 4), (34, 2), (36, 3),
)
ORDERS = {
    "stream": [A, B, C, D, E, F, G, H, I, J],
    "widest": [A, E, B, C, G, H, F, J, D, I],
    "twogroups": [A, E, B, F, C, G, D, H, I, J],
}
SIGNS = {
    "neg": [-1] * 10,
    "altneg": [-1, 1] * 5,
    "altpos": [1, -1] * 5,
    "pos": [1] * 10,
}
MAPS = ["even", "logarea", "arcsine"]
# Bits that follow the loudness of the frame (one in each excitation block).
LOUD = [80, 118, 140, 162, 184]
RATE = 8000
FRAME = 160


def field(bits, start, width, twos):
    """A field as a number from -1 to 1 (the middle of each step)."""
    v = sum(int(b) << i for i, b in enumerate(bits[start:start + width]))
    if twos and v >= 1 << (width - 1):
        v -= 1 << width
    if twos:
        return (v + 0.5) / (1 << (width - 1))
    return (v + 0.5) / (1 << width) * 2.0 - 1.0


def coefficients(bits, order, signs, mapping, c_twos):
    out = []
    for (start, width), sign in zip(order, signs):
        x = field(bits, start, width, c_twos and (start, width) == C)
        if mapping == "logarea":
            x = np.tanh(2.0 * x)
        elif mapping == "arcsine":
            x = np.sin(x * np.pi / 2)
        out.append(float(np.clip(sign * x, -0.98, 0.98)))
    return out


@njit(cache=True)
def lattice(k_frames, excitation, live):
    """An all-pole lattice filter, its coefficients moved once a frame."""
    out = np.zeros(len(excitation))
    state = np.zeros(10)
    k_old = np.zeros(10)
    for n in range(len(k_frames)):
        if not live[n]:
            k_old[:] = 0.0
            continue
        for i in range(160):
            mix = min(1.0, i / 40.0)
            f = excitation[n * 160 + i]
            for m in range(9, -1, -1):
                k = k_old[m] + (k_frames[n, m] - k_old[m]) * mix
                f -= k * state[m]
                if m < 9:
                    state[m + 1] = state[m] + k * f
            state[0] = f
            out[n * 160 + i] = f
        k_old[:] = k_frames[n]
    return out


def write(path, samples):
    peak = np.percentile(np.abs(samples), 99.5) or 1.0
    pcm = np.clip(samples / peak * 20000, -32767, 32767).astype(np.int16)
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(pcm.tobytes())


def main():
    frames = np.load(sys.argv[1]).astype(np.uint8)
    out_dir = sys.argv[2]
    seconds = float(sys.argv[3]) if len(sys.argv) > 3 else 12.0
    # The frame sent most often is the codec idling: silence.
    as_bytes = [bytes(f) for f in frames]
    idle = max(set(as_bytes), key=as_bytes.count)
    live_all = np.array([b != idle for b in as_bytes])
    loud_all = np.array([0.25 + sum(int(f[i]) for i in LOUD) / len(LOUD) for f in frames])
    # The liveliest stretch of that length.
    n = min(len(frames), int(seconds * 50))
    score = np.convolve(live_all * loud_all, np.ones(n), "valid")
    first = int(score.argmax())
    frames, live, loud = frames[first:first + n], live_all[first:first + n], loud_all[first:first + n]
    print(f"frames {first} to {first + n} ({first / 50:.1f} s in), {int(live.sum())} of them live")

    rng = np.random.default_rng(1)
    excitation = rng.standard_normal(n * FRAME) * np.repeat(loud, FRAME)
    rows = []
    combos = itertools.product(ORDERS, SIGNS, MAPS, [False, True])
    for number, (order, signs, mapping, c_twos) in enumerate(combos, 1):
        k = np.array([coefficients(f, ORDERS[order], SIGNS[signs], mapping, c_twos) for f in frames])
        name = f"t{number:02d}_{order}_{signs}_{mapping}{'_c2' if c_twos else ''}.wav"
        write(f"{out_dir}/{name}", lattice(k, excitation, live))
        rows.append((number, name, order, signs, mapping, "two's complement" if c_twos else "offset"))
    with open(f"{out_dir}/index.html", "w", encoding="utf-8") as page:
        page.write("<!doctype html><meta charset=utf-8><title>Voice trials</title>"
                   "<style>body{font:14px sans-serif;background:#111;color:#ddd}"
                   "td{padding:2px 10px}audio{height:28px}</style>"
                   "<h3>Voice trials: hiss through each guess at the predictor</h3>"
                   "<p>Order of the fields · signs · spacing of the values · how field 12-15 is read</p><table>")
        for number, name, order, signs, mapping, c in rows:
            page.write(f"<tr><td>{number}</td><td>{order}</td><td>{signs}</td><td>{mapping}</td><td>{c}</td>"
                       f"<td><audio controls preload=none src='{name}'></audio></td></tr>")
        page.write("</table>")
    print(len(rows), "trials and index.html written to", out_dir)


if __name__ == "__main__":
    main()

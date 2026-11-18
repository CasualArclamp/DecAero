# made by CasualArclamp
"""Sound from a 21000 bit/s call's saved voice frames: the best guess so far.

The 9.6 kbit/s codec (British Telecom's multipulse LPC) has no public
description of its bits. What is used here was worked out from one recorded
call (docs/DESIGN.md): which bits are the predictor's coefficients, the pitch
predictor and the pulses, with quantiser tables that are guesses
(scripts/c21_tables.json). Expect a voice-like sound, not clear speech.

    python scripts/c21_render.py voice21k_XXXX.bin OUT.wav [hiss|pulses|pitch]

The input is what DecAero saves in the `21k` folder beside the calls: 24
octets a frame, 50 frames a second. `pitch` (pulses and the pitch predictor)
is the default; `pulses` leaves the pitch predictor out; `hiss` drives the
filter with noise shaped by the pulses' loudness.
"""
import json
import os
import sys
import wave

import numpy as np

try:
    from numba import njit
except ImportError:  # slower, but it works
    def njit(*_args, **_kwargs):
        return lambda f: f

RATE = 8000
FRAME = 160
HERE = os.path.dirname(os.path.abspath(__file__))


def field(bits, first, width, msb_first=False):
    weights = 1 << (np.arange(width - 1, -1, -1) if msb_first else np.arange(width))
    return (bits[:, first:first + width] @ weights).astype(np.int64)


def checks_pass(bits):
    """Frames whose protected predictor bits agree with their check bits.

    The twelve parity relations found in the first 56 bits: each row lists
    the bits whose sum must be even (the last row, odd).
    """
    rows = [
        [1, 3, 4, 7, 9, 11, 13, 14, 15, 21, 25, 29, 38, 40, 42],
        [1, 2, 4, 5, 7, 9, 10, 13, 14, 17, 20, 25, 28, 35, 37, 43],
        [0, 4, 5, 13, 15, 20, 21, 22, 24, 37, 38, 40, 44],
        [0, 1, 2, 3, 13, 14, 15, 16, 24, 25, 28, 29, 35, 37, 38, 45],
        [6, 9, 10, 11, 14, 16, 20, 21, 22, 24, 25, 28, 29, 37, 38, 40, 46],
        [0, 2, 3, 5, 6, 7, 10, 11, 13, 16, 17, 22, 25, 37, 38, 47],
        [2, 4, 5, 9, 10, 13, 14, 15, 17, 22, 24, 25, 28, 35, 41, 51],
        [0, 2, 4, 5, 6, 9, 13, 14, 15, 17, 20, 21, 22, 24, 25, 35, 37, 38, 52],
        [0, 1, 2, 7, 10, 13, 14, 16, 20, 24, 25, 29, 35, 38, 41, 53],
        [4, 6, 9, 10, 14, 16, 20, 21, 22, 24, 25, 28, 37, 38, 40, 54],
        [0, 2, 3, 6, 10, 11, 15, 17, 20, 22, 25, 38, 41, 55],
    ]
    good = np.ones(len(bits), dtype=bool)
    for row in rows:
        good &= bits[:, row].sum(1) % 2 == 0
    good &= bits[:, [6, 7, 14, 15, 17, 20, 35, 37, 40, 41]].sum(1) % 2 == 1
    return good


@njit(cache=True)
def pitch_predictor(excitation, lag, gain):
    out = excitation.copy()
    for n in range(len(lag)):
        for i in range(n * 160, (n + 1) * 160):
            if i - lag[n] >= 0:
                out[i] += gain[n] * out[i - lag[n]]
    return out


@njit(cache=True)
def lattice(k_frames, excitation):
    """An all-pole lattice filter, its coefficients moved once a frame."""
    out = np.zeros(len(excitation))
    state = np.zeros(10)
    k_old = np.zeros(10)
    for n in range(len(k_frames)):
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


def render(bits, tables, kind="pitch"):
    n = len(bits)
    good = checks_pass(bits)
    # Predictor: a frame that fails its checks takes the one before it.
    k = np.zeros((n, 10))
    for t in tables["fields"]:
        k[:, t["coefficient"] - 1] = np.array(t["values"])[field(bits, t["first_bit"], t["bits"])]
    for i in range(1, n):
        if not good[i]:
            k[i] = k[i - 1]
    # Pulses: six groups of three, each a level and a place. Signs are not
    # known, so all are positive.
    excitation = np.zeros(n * FRAME)
    starts = [int(round(g * FRAME / 6)) for g in range(7)]
    step_db = tables["pulses"]["step_db"]
    for slot, group in enumerate(tables["pulses"]["groups"]):
        a, b = starts[slot], starts[slot + 1]
        for (level_at, level_bits), (place_at, place_bits) in group:
            level = 10.0 ** (field(bits, level_at, level_bits, True) * step_db / 20.0)
            place = field(bits, place_at, place_bits, True) * (1 if place_bits == 5 else 2)
            np.add.at(excitation, np.arange(n) * FRAME + a + np.minimum(place, b - a - 1), level)
    if kind == "hiss":
        loud = np.sqrt((excitation.reshape(-1, FRAME) ** 2).mean(1))
        excitation = np.random.default_rng(1).standard_normal(n * FRAME) * np.repeat(loud, FRAME)
    elif kind == "pitch":
        p = tables["pitch"]
        lag = p["lag_offset"] + field(bits, p["lag_first_bit"], p["lag_bits"])
        gain = np.array(p["gains"])[field(bits, p["gain_first_bit"], p["gain_bits"])]
        excitation = pitch_predictor(excitation, lag, gain)
    return lattice(np.clip(k, -0.98, 0.98), excitation)


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        return
    raw = np.fromfile(sys.argv[1], dtype=np.uint8)
    raw = raw[: len(raw) // 24 * 24].reshape(-1, 24)
    bits = np.unpackbits(raw, axis=1, bitorder="little")
    with open(os.path.join(HERE, "c21_tables.json")) as f:
        tables = json.load(f)
    kind = sys.argv[3] if len(sys.argv) > 3 else "pitch"
    samples = render(bits, tables, kind)
    # Level by the loud parts, with the silent frames left silent.
    peak = np.percentile(np.abs(samples), 99.7) or 1.0
    pcm = np.clip(samples / peak * 22000, -32767, 32767).astype(np.int16)
    with wave.open(sys.argv[2], "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(pcm.tobytes())
    print(f"{len(bits)} frames, {len(bits) / 50:.0f} s, to {sys.argv[2]} ({kind})")


if __name__ == "__main__":
    main()

# made by CasualArclamp
"""Trial recordings from the 21000 bit/s channel's voice frames.

The 9.6 kbit/s codec's quantisers are not known. This makes sound from a
guess at the predictor alone (the first 40 bits of each 192-bit frame, taken
as ten reflection coefficients), driven by hiss in place of the real pulses:
if a guess is right the result is whispered speech, if wrong it is a steady
or wandering noise with no words in it. One WAV a guess, to listen to.

    python scripts/c21_trials.py FRAMES.npy OUT_DIR

FRAMES.npy: frames x 192 bits (0/1), in time order.
"""
import sys
import wave

import numpy as np

# Ten fields in bits 0..39, as their statistics suggest (docs/DESIGN.md).
FIELDS = [(0, 2), (2, 6), (8, 4), (12, 6), (18, 5), (23, 3), (26, 4), (30, 6), (36, 3), (39, 1)]
# Bits that follow the loudness of the frame (one in each excitation block).
LOUD = [80, 118, 140, 162, 184]
RATE = 8000
FRAME = 160


def value(bits, start, width, lsb_first):
    """A field as a number from -1 to 1 (the middle of each step)."""
    field = bits[start:start + width]
    if not lsb_first:
        field = field[::-1]
    v = sum(int(b) << i for i, b in enumerate(field))
    return (v + 0.5) / (1 << width) * 2.0 - 1.0


def coefficients(bits, guess):
    out = []
    for start, width in guess["fields"]:
        x = value(bits, start, width, guess["lsb_first"])
        if guess["twos"]:
            # Two's complement: the top half of the codes are the negatives.
            x = x - 1.0 if x >= 0 else x + 1.0
        if guess["lar"]:
            # Log area ratios: finer steps near +-1.
            x = np.tanh(x * guess["lar"])
        out.append(float(np.clip(x * guess["sign"], -0.98, 0.98)))
    order = guess.get("order")
    return [out[i] for i in order] if order else out


def synthesise(frames, guess, seed=1):
    """An all-pole lattice filter, its coefficients moved once a frame."""
    rng = np.random.default_rng(seed)
    idle_frame = max(set(map(bytes, frames)), key=list(map(bytes, frames)).count)
    state = np.zeros(10)
    out = np.zeros(len(frames) * FRAME)
    k_old = np.zeros(10)
    for n, bits in enumerate(frames):
        if bytes(bits) == idle_frame:
            k_old = np.zeros(10)
            continue
        k_new = np.array(coefficients(bits, guess))
        loud = 0.25 + sum(int(bits[i]) for i in LOUD) / len(LOUD)
        excitation = rng.standard_normal(FRAME) * loud
        if guess["buzz"]:
            # A 110 Hz buzz with the hiss, for a voice-like sound.
            t = np.arange(n * FRAME, (n + 1) * FRAME)
            excitation = 0.4 * excitation + 4.0 * loud * (t % 73 == 0)
        for i in range(FRAME):
            # Coefficients glide from the old frame's to the new.
            k = k_old + (k_new - k_old) * min(1.0, i / 40.0)
            f = excitation[i]
            for m in range(9, -1, -1):
                f -= k[m] * state[m]
                if m < 9:
                    state[m + 1] = state[m] + k[m] * f
            state[0] = f
            out[n * FRAME + i] = f
        k_old = k_new
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
    base = {"fields": FIELDS, "lsb_first": True, "twos": False, "lar": 0.0, "sign": 1.0, "buzz": False}
    guesses = {
        "1_plain": {},
        "2_sign_flipped": {"sign": -1.0},
        "3_log_area": {"lar": 2.0},
        "4_log_area_sign_flipped": {"lar": 2.0, "sign": -1.0},
        "5_twos_complement": {"twos": True},
        "6_msb_first": {"lsb_first": False},
        "7_widest_fields_first": {"order": [1, 3, 7, 4, 2, 6, 5, 8, 0, 9]},
        "8_widest_first_sign_flipped": {"order": [1, 3, 7, 4, 2, 6, 5, 8, 0, 9], "sign": -1.0},
        "9_plain_with_buzz": {"buzz": True},
    }
    for name, change in guesses.items():
        samples = synthesise(frames, {**base, **change})
        write(f"{out_dir}/trial_{name}.wav", samples)
        print(name, "written,", round(len(samples) / RATE, 1), "s")


if __name__ == "__main__":
    main()

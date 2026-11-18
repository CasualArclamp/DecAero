# made by CasualArclamp
"""Speech from a 21000 bit/s call's saved voice frames.

The 9.6 kbit/s codec is British Telecom's multipulse LPC, defined in ICAO
Annex 10 Volume III Part I, Appendix 7 to Chapter 4 ("Voice encoding
algorithm definition", first edition as amended to Amendment 80). This is
that decoder in Python, as a check on the Rust one (crates/decaero-core,
`mplpc`): tables A7-2 to A7-17 and the decoder of section 5.

    python scripts/c21_render.py voice21k_XXXX.bin OUT.wav

The input is what DecAero saves in the `21k` folder beside the calls: 24
octets a frame, 50 frames a second, the first bit sent in each octet's top
bit.
"""
import sys
import wave

import numpy as np

# Table A7-1's decision levels are sines of evenly spaced angles; the output
# levels (tables A7-2 to A7-11) are the midpoints of neighbouring decisions.
# (lowest angle, highest angle, bits) for coefficients 1 to 10.
RANGES = [(-0.85, 1.46, 6), (-1.32, 0.79, 6), (-0.67, 0.90, 5), (-1.08, 0.50, 5), (-0.61, 0.69, 4),
          (-0.78, 0.36, 4), (-0.48, 0.67, 3), (-0.68, 0.48, 3), (-0.44, 0.81, 2), (-0.52, 0.44, 2)]
LEVELS = []
for lo, hi, width in RANGES:
    edges = np.sin(lo + (hi - lo) * np.arange((1 << width) + 1) / (1 << width))
    LEVELS.append((edges[:-1] + edges[1:]) / 2)
# How many of each coefficient's top bits the Hamming code covers (table A7-18).
PROTECTED = [6, 5, 3, 3, 2, 2, 2, 2, 1, 0]
PITCH_GAIN = [0.1, 0.325, 0.6, 0.9]  # table A7-12
INV_QUANT = {4: [0.1284, 0.3881, 0.6568, 0.9424, 1.256, 1.618, 2.069, 2.733],  # table A7-14
             3: [0.2451, 0.7560, 1.344, 2.152]}  # table A7-15
ADAPT = {4: [0.75, 0.875, 0.875, 0.875, 1.0, 1.25, 1.5, 2.0], 3: [0.875, 0.875, 1.0, 1.5]}
ADJUST = [1.0, 1.0 / 0.625, 1.0 / 0.375]


def number(bits):
    v = 0
    for b in bits:
        v = v << 1 | int(b)
    return v


def split(frame):
    """A frame's line codes, in the order of table A7-17."""
    at = 0
    def take(n):
        nonlocal at
        at += n
        return frame[at - n:at]
    k = [take(w) for _, _, w in RANGES]
    hamming, burst = take(6), take(8)
    gain, delay = number(take(2)), 32 + number(take(6))
    pulses = []
    for _ in range(5):
        block = []
        for width in (4, 4, 3):
            place = number(take(5))
            block.append((place, take(width), width))
        pulses.append(block)
    return k, hamming, burst, gain, delay, pulses


def checks(k, hamming, burst):
    """(Hamming syndrome, parity fault, columns of the burst check that fail)."""
    word = [b for bits, n in zip(k, PROTECTED) for b in bits[:n]]
    ham, count, n = 0, 3, 4
    for b in word:  # section 4.10.3.1
        if b:
            ham ^= count
        count += 1
        if count == n:
            n *= 2
            count += 1
    syndrome = ham ^ number(hamming[1:])
    parity = (sum(word) + sum(hamming)) % 2 == 1  # the sixth bit makes the code word even
    matrix = np.array(list(word) + list(hamming) + [int(b) ^ m for b, m in zip(burst, [1, 0, 1, 0, 1, 0, 1, 0])])
    columns = int((matrix.reshape(5, 8).sum(0) % 2).sum())
    return syndrome, parity, columns


class Decoder:
    def __init__(self):
        self.gfact = 1.0
        self.maxadapt = False
        self.s2 = np.zeros(96 + 160)  # the excitation's past, for the pitch predictor
        self.out = np.zeros(10)  # the filter's past output, newest first
        self.last = None

    def amplitude(self, code, width, index):
        """Section 5.1.3.3: the pulse's size, and the step size moved on."""
        sign = -1.0 if code[0] else 1.0
        i = number(code[1:])
        top = (1 << (width - 1)) - 1
        value = sign * INV_QUANT[width][i] * self.gfact / ADJUST[index]
        self.gfact = self.gfact ** 0.98
        if i == top:
            if self.maxadapt:
                self.gfact *= ADAPT[width][top]
            self.maxadapt = True
        else:
            self.maxadapt = False
        self.gfact = min(512.0, max(0.5, self.gfact * ADAPT[width][i]))
        return value

    def frame(self, bits):
        k, hamming, burst, gain, delay, pulses = split(bits)
        syndrome, parity, columns = checks(k, hamming, burst)
        bad = columns > 1 or (syndrome != 0 and not parity)
        if bad and self.last is not None:
            k, gain, delay, pulses = self.last  # repeat the frame before
        else:
            self.last = (k, gain, delay, pulses)
        refl = [LEVELS[i][number(k[i])] for i in range(10)]
        taps = list(refl)  # section 4.6: reflection coefficients to filter taps
        for i in range(1, 10):
            alpha = taps[:i]
            for j in range(i):
                taps[j] = alpha[j] - taps[i] * alpha[i - 1 - j]
        taps = np.array(taps)
        out = np.zeros(160)
        self.s2[:96] = self.s2[160:]
        for block in range(5):
            s1 = np.zeros(32)
            for index, (place, code, width) in enumerate(pulses[block]):
                s1[place] = self.amplitude(code, width, index)
            for n in range(32):
                at = 96 + block * 32 + n
                self.s2[at] = PITCH_GAIN[gain] * self.s2[at - delay] + s1[n]
                y = self.s2[at] + taps @ self.out
                self.out[1:] = self.out[:-1]
                self.out[0] = y
                out[block * 32 + n] = y
        return out, (syndrome, parity, columns)


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        return
    raw = np.fromfile(sys.argv[1], dtype=np.uint8)
    raw = raw[: len(raw) // 24 * 24].reshape(-1, 24)
    bits = np.unpackbits(raw, axis=1)
    decoder = Decoder()
    sound, clean = [], 0
    for frame in bits:
        samples, (syndrome, parity, columns) = decoder.frame(frame)
        clean += syndrome == 0 and columns == 0 and not parity
        sound.append(samples)
    samples = np.concatenate(sound)
    # The codec works on a 13-bit scale.
    pcm = np.clip(samples * 8, -32767, 32767).astype(np.int16)
    with wave.open(sys.argv[2], "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(8000)
        w.writeframes(pcm.tobytes())
    print(f"{len(bits)} frames ({clean} with every check good), {len(bits) / 50:.0f} s, to {sys.argv[2]}")


if __name__ == "__main__":
    main()

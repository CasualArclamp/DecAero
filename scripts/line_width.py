# made by CasualArclamp
"""How steady is a receiver's oscillator? Measured on an Aero P channel.

An A-BPSK (MSK-like) carrier squared has two spectral lines, half the bit
rate either side of twice its offset. With a steady oscillator they are as
narrow as the recording is long; an oscillator that wobbles smears them. This
prints the share of a line's power within a few widths of its peak (a steady
receiver has nearly all of it within 1 Hz) and, from that, a rough figure for
the wobble.

    python scripts/line_width.py RECORDING.wav CARRIER_HZ [BIT_RATE]

CARRIER_HZ is the carrier's offset from the recording's centre.
"""
import sys
import wave

import numpy as np


def main():
    path, fc = sys.argv[1], float(sys.argv[2])
    bit_rate = float(sys.argv[3]) if len(sys.argv) > 3 else 1200.0
    w = wave.open(path)
    fs, n, width = w.getframerate(), w.getnframes(), w.getsampwidth()
    n = min(n, int(fs * 40))
    raw = w.readframes(n)
    if width == 1:
        d = np.frombuffer(raw, dtype=np.uint8).astype(np.float32) - 127.5
    else:
        d = np.frombuffer(raw, dtype=np.int16).astype(np.float32)
    d = d.reshape(-1, 2)
    x = d[:, 0] + 1j * d[:, 1]
    # Down to about four times the bit rate, a block at a time (the big
    # recordings are 625 kS/s).
    dec = max(1, int(fs / (bit_rate * 6)))
    taps = np.sinc(np.arange(-8 * dec, 8 * dec + 1) / dec * 0.7) * np.hamming(16 * dec + 1)
    taps /= taps.sum()
    out = []
    block = dec * 20000
    phase0 = 0
    for k in range(0, len(x), block):
        seg = x[max(0, k - len(taps)):k + block + len(taps)]
        t = np.arange(max(0, k - len(taps)), max(0, k - len(taps)) + len(seg))
        y = np.convolve(seg * np.exp(-2j * np.pi * fc * t / fs), taps, 'same')
        start = k - max(0, k - len(taps))
        out.append(y[start:start + block][::dec])
    y = np.concatenate(out)
    fd = fs / dec
    s = y * y
    size = 1 << int(np.log2(len(s)))
    spectrum = np.abs(np.fft.fftshift(np.fft.fft(s[:size] * np.hanning(size)))) ** 2
    f = np.fft.fftshift(np.fft.fftfreq(size, 1 / fd))
    floor = np.median(spectrum)
    print(f'{path}: {size / fd:.0f} s, bins of {fd / size:.3f} Hz')
    for lo, hi in ((-bit_rate, 0.0), (0.0, bit_rate)):
        m = (f > lo) & (f < hi)
        peak = f[m][np.argmax(spectrum[m])]
        near = (f > peak - 100) & (f < peak + 100)
        total = spectrum[near].sum() - floor * near.sum()
        shares = []
        for wd in (0.3, 1, 3, 10, 30):
            inside = (f > peak - wd) & (f < peak + wd)
            shares.append(f'+-{wd:g} Hz {(spectrum[inside].sum() - floor * inside.sum()) / total:.2f}')
        over = 10 * np.log10(spectrum[m].max() / floor)
        print(f'  line at {peak:+8.2f} Hz, {over:4.1f} dB over the floor:  ' + '  '.join(shares))


main()

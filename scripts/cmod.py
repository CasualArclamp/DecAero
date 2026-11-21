# made by CasualArclamp
# Test modulator: 8400 bit/s C channel carriers (offset QPSK, root raised
# cosine) in white noise, as 16-bit IQ WAV at 1.248 MS/s.
# usage: python scripts/cmod.py OUT.wav SECONDS ROLLOFF i BITS.bin:OFFSET_HZ:START_S:ESN0_DB ...
# (BITS.bin from `cargo run -p decaero-core --example cbits`; offsets are from
# +200 kHz; QLAG "i" puts I half a symbol after Q, as the receiver expects)
import sys, wave, numpy as np
from scipy.signal import resample_poly
out, secs, alpha, qlag = sys.argv[1], float(sys.argv[2]), float(sys.argv[3]), sys.argv[4] == "q"
RS, SPS, UP, DOWN = 4200, 20, 104, 7
fs1 = RS*SPS; fs2 = fs1*UP//DOWN
def rrc(a, sps, span=12):
    t = np.arange(-span*sps, span*sps+1)/sps
    h = np.zeros_like(t)
    for i, x in enumerate(t):
        if abs(x) < 1e-9: h[i] = 1 - a + 4*a/np.pi
        elif a > 0 and abs(abs(x) - 1/(4*a)) < 1e-9:
            h[i] = a/np.sqrt(2)*((1+2/np.pi)*np.sin(np.pi/(4*a)) + (1-2/np.pi)*np.cos(np.pi/(4*a)))
        else:
            h[i] = (np.sin(np.pi*x*(1-a)) + 4*a*x*np.cos(np.pi*x*(1+a))) / (np.pi*x*(1-(4*a*x)**2))
    return h/np.sqrt(np.sum(h**2))
h = rrc(alpha, SPS)
n1 = int(secs*fs1)
base = np.zeros(n1, np.complex128)
t1 = np.arange(n1)/fs1
carriers = []
for spec in sys.argv[5:]:
    path, off, start, esn0 = spec.split(":")
    off, start, esn0 = float(off), float(start), float(esn0)
    bits = np.fromfile(path, np.uint8).astype(float)*2-1
    q, i = bits[0::2], bits[1::2]
    nsym = n1//SPS + 1
    q = np.resize(q, nsym); i = np.resize(i, nsym)
    ii = np.zeros(nsym*SPS); qq = np.zeros(nsym*SPS)
    ii[::SPS] = i
    # Offset QPSK: one rail half a symbol after the other.
    if qlag: qq[SPS//2::SPS] = q
    else:
        qq[::SPS] = q; ii[:] = 0; ii[SPS//2::SPS] = i
    s = np.convolve(ii + 1j*qq, h)[:n1]
    s /= np.sqrt(np.mean(np.abs(s)**2))
    s[:int(start*fs1)] = 0
    carriers.append((s*np.exp(2j*np.pi*off*t1), esn0))
# Noise: unit complex variance at fs2, so N0 = 1/fs2; a carrier of power P has Es/N0 = P*fs2/RS.
for s, esn0 in carriers:
    base += s*np.sqrt(10**(esn0/10)*RS/fs2)
y = resample_poly(base, UP, DOWN)
m = len(y)
y = (y*np.exp(2j*np.pi*200000*np.arange(m)/fs2)).astype(np.complex64)
rng = np.random.default_rng(7)
y += ((rng.standard_normal(m)+1j*rng.standard_normal(m))/np.sqrt(2)).astype(np.complex64)
y *= 3000/np.std(y)
iq = np.empty(2*m, np.int16); iq[0::2] = np.clip(y.real,-32767,32767); iq[1::2] = np.clip(y.imag,-32767,32767)
o = wave.open(out, 'wb'); o.setnchannels(2); o.setsampwidth(2); o.setframerate(fs2); o.writeframes(iq.tobytes())
print(f"{m/fs2:.1f} s at {fs2}; carriers at +200 kHz +", [c.split(':')[1] for c in sys.argv[5:]])

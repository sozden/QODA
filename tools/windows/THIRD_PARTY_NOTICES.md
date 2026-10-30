# Third-party components

The Windows package of QODA includes the following third-party components.
Their license texts are in the `licenses` folder. QODA itself is licensed
under GPL-3.0-or-later (see `LICENSE`).

| Component | Files | License | Source |
|---|---|---|---|
| SoapySDR 0.8 (from PothosSDR 2021.07.25) | `SoapySDR.dll` | Boost Software License 1.0 | https://github.com/pothosware/SoapySDR |
| SoapyRTLSDR | `lib/SoapySDR/modules0.8/rtlsdrSupport.dll` | MIT | https://github.com/pothosware/SoapyRTLSDR |
| SoapyHackRF | `lib/SoapySDR/modules0.8/HackRFSupport.dll` | MIT | https://github.com/pothosware/SoapyHackRF |
| librtlsdr, RTL-SDR Blog release V1.4.0 | `rtlsdr.dll` | GPL-2.0-or-later | https://github.com/rtlsdrblog/rtl-sdr-blog |
| pthreads-win32 | `pthreadVC2.dll` | LGPL-2.1-or-later | https://sourceware.org/pthreads-win32/ |
| libhackrf (from PothosSDR 2021.07.25) | `hackrf.dll` | GPL-2.0-or-later | https://github.com/greatscottgadgets/hackrf |
| libusb (from PothosSDR 2021.07.25) | `libusb-1.0.dll` | LGPL-2.1-or-later | https://github.com/libusb/libusb |
| libiio 0.19 (from PothosSDR 2021.07.25) | `libiio.dll` | LGPL-2.1-or-later | https://github.com/analogdevicesinc/libiio |
| Microsoft Visual C++ runtime | `MSVCP140.dll`, `VCRUNTIME140.dll`, `VCRUNTIME140_1.dll`, `msvcr100.dll` | Microsoft redistributable | https://learn.microsoft.com/cpp/windows/latest-supported-vc-redist |

If the package contains an `ffmpeg` folder (the `-ffmpeg` zip), it also
includes FFmpeg (`ffmpeg.exe`, `ffplay.exe` and its DLLs), a shared GPL build
from https://github.com/BtbN/FFmpeg-Builds, licensed under GPL-3.0-or-later.
Source code is at https://ffmpeg.org/download.html; the license text is
`ffmpeg/LICENSE.txt` and build details are in `ffmpeg/README.txt`.
Otherwise QODA uses an FFmpeg installed on the system
(for example `winget install Gyan.FFmpeg`).

The binaries above are unmodified copies of the listed releases. Their source
code is available from the linked projects; on request the QODA maintainers
will also provide the exact source of the versions included here.

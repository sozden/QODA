# QODA

QO-100 uydusundaki DATV (DVB-S2) yayınlarını SDR alıcılarla izlemek için Rust ile yazılmış açık kaynak bir uygulama.

QODA, QO-100 ve DATV'den esinlenmiştir.

## Durum

Erken geliştirme aşamasında. İlk hedef: RTL-SDR, HackRF ve Pluto/LibreSDR cihazlarını SoapySDR üzerinden açıp spektrumu göstermek.

## Kullanım

1. Windows'ta [PothosSDR](https://downloads.myriadrf.org/builds/PothosSDR/) kurun ve `bin` klasörünü PATH'e ekleyin (SoapySDR ve RTL-SDR sürücüsü bununla gelir).
   PothosSDR 2021 ile gelen eski `rtlsdr.dll` akış durdurulurken çökebiliyor. [RTL-SDR Blog sürümlerinden](https://github.com/rtlsdrblog/rtl-sdr-blog/releases) `x64` klasöründeki `rtlsdr.dll`, `pthreadVC2.dll` ve `msvcr100.dll` dosyalarını `qoda.exe`'nin yanına koyun.
2. `cargo run --release` ile QODA'yı başlatın.
3. Cihazı seçip **Başlat**'a basın. LNB LO ve merkez RF frekansını girin; IF otomatik hesaplanır.
4. Spektrumda bir yayına **tıklayın** (seçer) ya da **çift tıklayın** (seçer ve alıcıyı oraya ortalar). Sembol hızı ve roll-off'a göre kaplanan bant mavi olarak gösterilir.

## Planlanan özellikler

- Yaygın SDR cihazlarını tanıma (SoapySDR)
- LNB LO ve yayın frekansından IF hesabı ve otomatik ayar
- Spektrum ve şelale görünümü, tıklayarak frekans seçimi
- Yayın üzerindeki sembol hızı, bant genişliği ve FEC'in otomatik tespiti
- Sıfırdan Rust ile yazılmış DVB-S2 çözücü (ETSI EN 302 307 standardı temel alınır)

## Lisans

GPL-3.0-or-later. Ayrıntılar için [LICENSE](LICENSE) dosyasına bakın.

## Katkıda bulunanlar

Bu proje, "yAtölye" (yeniden atölye) çalışma grubu tarafından geliştirilmektedir.

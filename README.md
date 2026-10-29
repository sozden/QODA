# QODA

QO-100 uydusundaki DATV (DVB-S2) yayınlarını SDR alıcılarla izlemek için Rust ile yazılmış açık kaynak bir uygulama.

QODA, QO-100 ve DATV'den esinlenmiştir.

## Durum

Erken geliştirme aşamasında. İlk hedef: RTL-SDR, HackRF ve Pluto/LibreSDR cihazlarını SoapySDR üzerinden açıp spektrumu göstermek.

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

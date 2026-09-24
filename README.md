# 🌸 Rinka SoundBridge

[!\[Python](https://img.shields.io/badge/Python-3.10%2B-blue?logo=python)](https://www.python.org/)
[!\[Platform](https://img.shields.io/badge/Platform-Windows%2010%20%2F%2011%20(64--bit)-0078D6?logo=windows)](https://microsoft.com)
[!\[License](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)
[!\[Framework](https://img.shields.io/badge/GUI-PyQt6-41CD52?logo=qt)](https://riverbankcomputing.com/software/pyqt/)

**Rinka SoundBridge** adalah aplikasi *standalone dual VST3 host* dan *real-time audio router* berbasis Windows WASAPI yang dirancang sangat ringan (Sepertinya, ya ga tau kok tanya saya). Aplikasi ini diciptakan khusus untuk memenuhi kebutuhan *live streaming*, VTubing, dan rantai pemrosesan *voice changer* eksternal *RINKA EMINA* (seperti Vonovox atau RVC) tanpa perlu membuka DAW penuh yang berat (Reaper, FL Studio, Ableton).

> \*\*Catatan Proyek:\*\*  
> Perangkat lunak ini awalnya dikembangkan secara mandiri dengan bantuan AI untuk memecahkan kebutuhan routing audio dan rantai \*voice changer\* \*pribadi\* (daripada membajak kan atau pakai 2 aplikasi beda). Repositori ini dibagikan secara terbuka (\*as-is\*) bagi siapa saja yang membutuhkan, just in case.

\---

## Tampilan Aplikasi

!\[Rinka SoundBridge Screenshot](preview.png)

\---

## Fitur Utama

* **Arsitektur Jalur Ganda Terisolasi (Dual-Line Architecture):**

  * **Line A:** Dikhususkan untuk mikrofon vokal untuk diproses VST sebelum masuk ke Voice Changer (Mic Fisik ➔ VST FX ➔ Virtual Cable / Voice Changer).
  * **Line B:** Dikhususkan untuk hasil olahan voice changer untuk diproses VST (Cable Out ➔ VST FX ➔ Headphone/Speaker).
* **Mesin Audio Native C++ DSP:** Seluruh pemrosesan plugin VST3 dieksekusi di lapisan C++ native melalui Spotify `pedalboard` (berbasis JUCE), membebaskan pemrosesan audio dari hambatan *Global Interpreter Lock* (GIL) Python.
* **Super Ringan \& Hemat Sumber Daya:**

  * Konsumsi RAM: \~45 MB – 85 MB.
  * Beban CPU Standby: < 1% (menggunakan alokasi buffer statis `numpy.float32` *zero-copy*).
* **System Tray Mode (0% GPU Usage):** Saat jendela disembunyikan ke System Tray Windows, seluruh *event loop* antarmuka PyQt6 dan animasi VU meter dibekukan total (0% GPU), namun mesin audio C++ tetap berjalan 100% di latar belakang.
* **Hotplug \& Refresh Audio Device:** Deteksi penambahan microphone atau headset USB baru secara langsung tanpa perlu menutup atau me-restart aplikasi.
* **End-to-End Latency Probe:** Mengukur latensi riil *roundtrip* dari saat Anda berbicara di mic hingga suara hasil olahan *voice changer* keluar ke speaker menggunakan algoritma korelasi silang selubung (*Envelope Cross-Correlation*). (ini khusus voice changer, jadi setidaknya bisa mengukur latency asli dari Mic sampai ke output final, walau masih estimasi kasaran kurang-lebih bisa dipakai)
* **Perbaikan Driver WASAPI Khusus (SSL 2 Fix):** Memisahkan *stream duplex* input-output secara mandiri dan mendukung *auto-upmixing* mono ke stereo untuk mencegah crash `\[Errno -9998]` pada audio interface profesional.
* **Preset Multi-Tema:** Dilengkapi beberapa theme sebagai pilihan.
* **Fungsi on-off setiap line jika tidak dibutuhkan, beserta fungsi mute**
* **Visual audio bar diagonal**
* **Fungsi menambahkan audio layer di line A dan B jika dibutuhkan (pada kasus Rinka Emina, dipakai untuk memasukkan noise Ruangan)**

\---

## Kebutuhan Sistem

* **Sistem Operasi:** Windows 10 atau Windows 11 (Wajib **64-bit**).
* **Python:** Python 3.10 atau versi yang lebih baru (Wajib versi 64-bit).
* **Driver Tambahan (Opsional):** *muzychenko vac* jika ingin menghubungkan audio antar-aplikasi ke OBS Studio atau Discord.

\---

## Panduan Instalasi \& Penggunaan

### 1\. Klon Repositori

Buka terminal (Command Prompt atau PowerShell), lalu jalankan:

```bash
git clone https://github.com/Rinka-Emina/Rinka-SoundBridge.git
cd Rinka-SoundBridge





### 2\. Install Requirements

Jika ingin meng-install Requirements

```bash

pip install -r requirements.txt



### 3\. Run

Jika ingin menjalankan dari folder

```bash

jalankan run.bat



### 4\. BUILD

jika ingin menjadikan file siap pakai (exe)
```bash
jalankan Build\_exe.bat


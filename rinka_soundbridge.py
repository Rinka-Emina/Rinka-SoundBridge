"""
Rinka SoundBridge v1.21
========================
Dual-line audio routing dan VST3 host untuk keperluan live streaming/VTubing
berbasis WASAPI (Windows).

Changelog v1.21:
- Sistem Hotplug & Refresh Audio Device mandiri tanpa perlu me-restart aplikasi.
- Smart Name Matching saat refresh perangkat baru agar pilihan tidak tertukar.
- Safe Delayed Process Restart untuk mencegah tabrakan driver WASAPI (Anti-Crash).
- Centralized clean_shutdown() untuk menghentikan timer UI dan melepas COM PortAudio.
"""

import sys
import os
import json
import time
import threading
import subprocess
import datetime
import queue
import numpy as np
import sounddevice as sd
from pedalboard import load_plugin
from pedalboard.io import AudioFile

from PyQt6.QtWidgets import (
    QApplication, QMainWindow, QWidget, QVBoxLayout, QHBoxLayout,
    QLabel, QComboBox, QPushButton, QSlider, QScrollArea, QFrame,
    QFileDialog, QSystemTrayIcon, QMenu, QMessageBox, QDialog,
    QLineEdit, QListWidget, QListWidgetItem, QAbstractItemView, QSizePolicy,
    QCheckBox, QGroupBox
)
from PyQt6.QtCore import Qt, QTimer, pyqtSignal, QByteArray
from PyQt6.QtGui import QFont, QAction, QColor, QPainter

# Win32 API Interop dengan deklarasi tipe 64-bit lengkap
if sys.platform == "win32":
    import ctypes
    from ctypes import wintypes
    user32 = ctypes.windll.user32

    WNDENUMPROC = ctypes.WINFUNCTYPE(ctypes.c_bool, wintypes.HWND, wintypes.LPARAM)

    user32.IsWindowVisible.argtypes = [wintypes.HWND]
    user32.IsWindowVisible.restype = wintypes.BOOL

    user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
    user32.GetWindowThreadProcessId.restype = wintypes.DWORD

    user32.EnumWindows.argtypes = [WNDENUMPROC, wintypes.LPARAM]
    user32.EnumWindows.restype = wintypes.BOOL

    user32.GetWindowLongW.argtypes = [wintypes.HWND, ctypes.c_int]
    user32.GetWindowLongW.restype = wintypes.LONG

    user32.SetWindowLongW.argtypes = [wintypes.HWND, ctypes.c_int, wintypes.LONG]
    user32.SetWindowLongW.restype = wintypes.LONG

    user32.SetWindowTextW.argtypes = [wintypes.HWND, wintypes.LPCWSTR]
    user32.SetWindowTextW.restype = wintypes.BOOL

    user32.GetWindowRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
    user32.GetWindowRect.restype = wintypes.BOOL

    user32.GetSystemMetrics.argtypes = [ctypes.c_int]
    user32.GetSystemMetrics.restype = ctypes.c_int

    user32.SetWindowPos.argtypes = [
        wintypes.HWND, wintypes.HWND,
        ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int,
        ctypes.c_uint
    ]
    user32.SetWindowPos.restype = wintypes.BOOL

def get_base_dir():
    if getattr(sys, "frozen", False):
        return os.path.dirname(sys.executable)
    return os.path.dirname(os.path.abspath(__file__))

CONFIG_FILE = os.path.join(get_base_dir(), "rinka_soundbridge_config.json")
LOG_DIR = os.path.join(get_base_dir(), "logs")

# ==============================================================================
# 1. SISTEM TEMA
# ==============================================================================

THEMES = {
    "Sakura Afternoon": {
        "BG_APP": "#FBF2E4", "BG_PANEL": "#FFF9F0", "BG_PANEL_ALT": "#F6E9D6",
        "BG_INPUT": "#FFFDF8", "BG_SCROLL": "#FFFCF6",
        "BORDER": "#E6D3B0", "BORDER_STRONG": "#D6BD8E",
        "TEXT_PRIMARY": "#5C4A3D", "TEXT_SECONDARY": "#9C8874", "TEXT_MUTED": "#B7A78E",
        "ACCENT_PINK": "#E8A0AC", "ACCENT_PINK_SOFT": "#F3C9D1", "ACCENT_PINK_HOVER": "#EEB3BE",
        "ACCENT_GREEN": "#9DC3A0", "ACCENT_GREEN_HOVER": "#AFD1B2", "ACCENT_GREEN_DEEP": "#7CAA80",
        "ACCENT_BLUE": "#A9C9DC", "ACCENT_BLUE_HOVER": "#BBD6E6", "ACCENT_BLUE_DEEP": "#8AB4CC",
        "ACCENT_CORAL": "#E3968A", "ACCENT_CORAL_HOVER": "#EAA89D", "ACCENT_CORAL_DEEP": "#D07C6F",
        "ACCENT_YELLOW": "#EAD08C", "ACCENT_LAVENDER": "#C9B7DD", "ACCENT_LAVENDER_HOVER": "#D6C7E6",
        "SHADOW": "#E6D3B0",
    },
    "Matcha Garden": {
        "BG_APP": "#F3F3E3", "BG_PANEL": "#FAFAF0", "BG_PANEL_ALT": "#E9E9CE",
        "BG_INPUT": "#FCFCF3", "BG_SCROLL": "#FBFBF0",
        "BORDER": "#CBD8A8", "BORDER_STRONG": "#B4C68A",
        "TEXT_PRIMARY": "#465232", "TEXT_SECONDARY": "#7C8C63", "TEXT_MUTED": "#A0AD8A",
        "ACCENT_PINK": "#D8A9A0", "ACCENT_PINK_SOFT": "#EFD3CD", "ACCENT_PINK_HOVER": "#E3BAB1",
        "ACCENT_GREEN": "#8CAE6A", "ACCENT_GREEN_HOVER": "#9EBD7E", "ACCENT_GREEN_DEEP": "#6E9350",
        "ACCENT_BLUE": "#A8C6B8", "ACCENT_BLUE_HOVER": "#BAD4C6", "ACCENT_BLUE_DEEP": "#8CB2A0",
        "ACCENT_CORAL": "#D19478", "ACCENT_CORAL_HOVER": "#DBA98F", "ACCENT_CORAL_DEEP": "#B87A5E",
        "ACCENT_YELLOW": "#DCCB7E", "ACCENT_LAVENDER": "#BFC894", "ACCENT_LAVENDER_HOVER": "#CCD4A9",
        "SHADOW": "#CBD8A8",
    },
    "Seifuku Blue": {
        "BG_APP": "#EEF3F8", "BG_PANEL": "#FAFCFE", "BG_PANEL_ALT": "#DCE7F0",
        "BG_INPUT": "#FCFDFF", "BG_SCROLL": "#FBFDFF",
        "BORDER": "#B9CEE0", "BORDER_STRONG": "#9BB9D3",
        "TEXT_PRIMARY": "#33455A", "TEXT_SECONDARY": "#6E839C", "TEXT_MUTED": "#9AACC0",
        "ACCENT_PINK": "#D9A7B8", "ACCENT_PINK_SOFT": "#EFD3DE", "ACCENT_PINK_HOVER": "#E3BBC9",
        "ACCENT_GREEN": "#9BC0B0", "ACCENT_GREEN_HOVER": "#ADCEBF", "ACCENT_GREEN_DEEP": "#7BA692",
        "ACCENT_BLUE": "#7FA8CC", "ACCENT_BLUE_HOVER": "#94B8D6", "ACCENT_BLUE_DEEP": "#5E8DB8",
        "ACCENT_CORAL": "#D68E8E", "ACCENT_CORAL_HOVER": "#E0A3A3", "ACCENT_CORAL_DEEP": "#BD7373",
        "ACCENT_YELLOW": "#E3D08F", "ACCENT_LAVENDER": "#AEB8DC", "ACCENT_LAVENDER_HOVER": "#C0C8E4",
        "SHADOW": "#B9CEE0",
    },
    "Golden Hour": {
        "BG_APP": "#FBEEDD", "BG_PANEL": "#FFF6EA", "BG_PANEL_ALT": "#F5DEBE",
        "BG_INPUT": "#FFF9F0", "BG_SCROLL": "#FFF7EC",
        "BORDER": "#EACB9A", "BORDER_STRONG": "#DDB57A",
        "TEXT_PRIMARY": "#5E3F26", "TEXT_SECONDARY": "#9C7750", "TEXT_MUTED": "#BE9C74",
        "ACCENT_PINK": "#E8A583", "ACCENT_PINK_SOFT": "#F5CDB8", "ACCENT_PINK_HOVER": "#EFBA9E",
        "ACCENT_GREEN": "#A8B979", "ACCENT_GREEN_HOVER": "#B8C78F", "ACCENT_GREEN_DEEP": "#8CA05C",
        "ACCENT_BLUE": "#93B4C4", "ACCENT_BLUE_HOVER": "#A8C4D2", "ACCENT_BLUE_DEEP": "#749CAE",
        "ACCENT_CORAL": "#E08159", "ACCENT_CORAL_HOVER": "#E89873", "ACCENT_CORAL_DEEP": "#C4693F",
        "ACCENT_YELLOW": "#F0C468", "ACCENT_LAVENDER": "#D9AE8E", "ACCENT_LAVENDER_HOVER": "#E3C0A5",
        "SHADOW": "#EACB9A",
    },
    "Lavender Twilight": {
        "BG_APP": "#F1EEF6", "BG_PANEL": "#F9F7FC", "BG_PANEL_ALT": "#E1DAED",
        "BG_INPUT": "#FBF9FD", "BG_SCROLL": "#FAF8FC",
        "BORDER": "#CFC0E2", "BORDER_STRONG": "#B7A2D4",
        "TEXT_PRIMARY": "#453655", "TEXT_SECONDARY": "#7F6E96", "TEXT_MUTED": "#A897BC",
        "ACCENT_PINK": "#D9A0C0", "ACCENT_PINK_SOFT": "#EFD1E3", "ACCENT_PINK_HOVER": "#E3B5D1",
        "ACCENT_GREEN": "#A0B6A8", "ACCENT_GREEN_HOVER": "#B2C6B9", "ACCENT_GREEN_DEEP": "#829D8B",
        "ACCENT_BLUE": "#A2A9DC", "ACCENT_BLUE_HOVER": "#B6BCE4", "ACCENT_BLUE_DEEP": "#8890C8",
        "ACCENT_CORAL": "#C591A8", "ACCENT_CORAL_HOVER": "#D1A5B8", "ACCENT_CORAL_DEEP": "#AC7690",
        "ACCENT_YELLOW": "#DCC98F", "ACCENT_LAVENDER": "#B69EDC", "ACCENT_LAVENDER_HOVER": "#C6B3E4",
        "SHADOW": "#CFC0E2",
    },
    "Mint Soda": {
        "BG_APP": "#EDF7F3", "BG_PANEL": "#F8FEFB", "BG_PANEL_ALT": "#D9F0E6",
        "BG_INPUT": "#FAFFFC", "BG_SCROLL": "#F9FEFC",
        "BORDER": "#B9E3D2", "BORDER_STRONG": "#98D4BC",
        "TEXT_PRIMARY": "#2E5A4C", "TEXT_SECONDARY": "#66968A", "TEXT_MUTED": "#93BBAF",
        "ACCENT_PINK": "#E8AFC4", "ACCENT_PINK_SOFT": "#F6D7E2", "ACCENT_PINK_HOVER": "#EFC1D3",
        "ACCENT_GREEN": "#71C4A0", "ACCENT_GREEN_HOVER": "#87D0B1", "ACCENT_GREEN_DEEP": "#4FA97F",
        "ACCENT_BLUE": "#8FCBD9", "ACCENT_BLUE_HOVER": "#A5D6E1", "ACCENT_BLUE_DEEP": "#69B4C6",
        "ACCENT_CORAL": "#E39C93", "ACCENT_CORAL_HOVER": "#EAB1A9", "ACCENT_CORAL_DEEP": "#CE7C71",
        "ACCENT_YELLOW": "#E8DA8C", "ACCENT_LAVENDER": "#B8D4E0", "ACCENT_LAVENDER_HOVER": "#CBE0E9",
        "SHADOW": "#B9E3D2",
    },
    "Cherry Blossom Night": {
        "BG_APP": "#2B2333", "BG_PANEL": "#352C40", "BG_PANEL_ALT": "#3F3450",
        "BG_INPUT": "#3A3049", "BG_SCROLL": "#372D46",
        "BORDER": "#584A6B", "BORDER_STRONG": "#6C5A82",
        "TEXT_PRIMARY": "#F0E4EE", "TEXT_SECONDARY": "#C3AFD4", "TEXT_MUTED": "#9683AD",
        "ACCENT_PINK": "#E8A0C0", "ACCENT_PINK_SOFT": "#5A4060", "ACCENT_PINK_HOVER": "#F0B5CC",
        "ACCENT_GREEN": "#8FCBA8", "ACCENT_GREEN_HOVER": "#A3D6B9", "ACCENT_GREEN_DEEP": "#6EB088",
        "ACCENT_BLUE": "#93B0E0", "ACCENT_BLUE_HOVER": "#A8C0E8", "ACCENT_BLUE_DEEP": "#7092CC",
        "ACCENT_CORAL": "#E28FA0", "ACCENT_CORAL_HOVER": "#E8A3B2", "ACCENT_CORAL_DEEP": "#CC6E82",
        "ACCENT_YELLOW": "#E3CE84", "ACCENT_LAVENDER": "#B39BE0", "ACCENT_LAVENDER_HOVER": "#C4B0E8",
        "SHADOW": "#1A1522",
    },
    "Vanilla Sky": {
        "BG_APP": "#F5F8FC", "BG_PANEL": "#FCFDFF", "BG_PANEL_ALT": "#E4EDF7",
        "BG_INPUT": "#FDFEFF", "BG_SCROLL": "#FCFEFF",
        "BORDER": "#CDDCEC", "BORDER_STRONG": "#AFC6DE",
        "TEXT_PRIMARY": "#40495C", "TEXT_SECONDARY": "#7C8CA3", "TEXT_MUTED": "#A9B7C9",
        "ACCENT_PINK": "#E6B0BC", "ACCENT_PINK_SOFT": "#F5DBE1", "ACCENT_PINK_HOVER": "#EEC5CD",
        "ACCENT_GREEN": "#A9CCBE", "ACCENT_GREEN_HOVER": "#BAD7CC", "ACCENT_GREEN_DEEP": "#88B4A2",
        "ACCENT_BLUE": "#9AC1E0", "ACCENT_BLUE_HOVER": "#AFCEE6", "ACCENT_BLUE_DEEP": "#78A9D0",
        "ACCENT_CORAL": "#E0A79B", "ACCENT_CORAL_HOVER": "#E8BAB0", "ACCENT_CORAL_DEEP": "#C88A7D",
        "ACCENT_YELLOW": "#EBDD9E", "ACCENT_LAVENDER": "#C2C9EA", "ACCENT_LAVENDER_HOVER": "#D2D8F0",
        "SHADOW": "#CDDCEC",
    },
    "Autumn Maple": {
        "BG_APP": "#FAEEE6", "BG_PANEL": "#FFF7F1", "BG_PANEL_ALT": "#F3DCCB",
        "BG_INPUT": "#FFF9F5", "BG_SCROLL": "#FEF7F2",
        "BORDER": "#E6C2A0", "BORDER_STRONG": "#D6A374",
        "TEXT_PRIMARY": "#5C3626", "TEXT_SECONDARY": "#93664C", "TEXT_MUTED": "#BC9375",
        "ACCENT_PINK": "#D9847A", "ACCENT_PINK_SOFT": "#EFC0B8", "ACCENT_PINK_HOVER": "#E39D93",
        "ACCENT_GREEN": "#9FAE68", "ACCENT_GREEN_HOVER": "#B0BE7E", "ACCENT_GREEN_DEEP": "#84934C",
        "ACCENT_BLUE": "#94AAAE", "ACCENT_BLUE_HOVER": "#A8BBBE", "ACCENT_BLUE_DEEP": "#79949A",
        "ACCENT_CORAL": "#CC5F42", "ACCENT_CORAL_HOVER": "#D77B60", "ACCENT_CORAL_DEEP": "#B04A2F",
        "ACCENT_YELLOW": "#E6B562", "ACCENT_LAVENDER": "#C99A82", "ACCENT_LAVENDER_HOVER": "#D6AE98",
        "SHADOW": "#E6C2A0",
    },
    "Wisteria Rain": {
        "BG_APP": "#EDEFF6", "BG_PANEL": "#F8F9FD", "BG_PANEL_ALT": "#DBDFEE",
        "BG_INPUT": "#FAFBFE", "BG_SCROLL": "#F9FAFD",
        "BORDER": "#C2C8E2", "BORDER_STRONG": "#A5AED4",
        "TEXT_PRIMARY": "#3B3F5C", "TEXT_SECONDARY": "#767CA0", "TEXT_MUTED": "#A3A8C4",
        "ACCENT_PINK": "#C9A0C7", "ACCENT_PINK_SOFT": "#E9D4E8", "ACCENT_PINK_HOVER": "#DAB8D8",
        "ACCENT_GREEN": "#9FB3C4", "ACCENT_GREEN_HOVER": "#B1C2D0", "ACCENT_GREEN_DEEP": "#7E97AC",
        "ACCENT_BLUE": "#8C9BDC", "ACCENT_BLUE_HOVER": "#A1AFE4", "ACCENT_BLUE_DEEP": "#6E7FC8",
        "ACCENT_CORAL": "#B592B0", "ACCENT_CORAL_HOVER": "#C4A6C1", "ACCENT_CORAL_DEEP": "#9B7797",
        "ACCENT_YELLOW": "#D4CB94", "ACCENT_LAVENDER": "#A996DC", "ACCENT_LAVENDER_HOVER": "#BCACE4",
        "SHADOW": "#C2C8E2",
    },
    "Windows 11 Dark": {
        "BG_APP": "#202020", "BG_PANEL": "#2C2C2C", "BG_PANEL_ALT": "#272727",
        "BG_INPUT": "#1E1E1E", "BG_SCROLL": "#232323",
        "BORDER": "#383838", "BORDER_STRONG": "#4A4A4A",
        "TEXT_PRIMARY": "#FFFFFF", "TEXT_SECONDARY": "#CCCCCC", "TEXT_MUTED": "#8A8A8A",
        "ACCENT_PINK": "#60CDFF", "ACCENT_PINK_SOFT": "#243E4F", "ACCENT_PINK_HOVER": "#78D5FF",
        "ACCENT_GREEN": "#6CCB5F", "ACCENT_GREEN_HOVER": "#84D779", "ACCENT_GREEN_DEEP": "#449E37",
        "ACCENT_BLUE": "#60CDFF", "ACCENT_BLUE_HOVER": "#78D5FF", "ACCENT_BLUE_DEEP": "#0078D4",
        "ACCENT_CORAL": "#FF6B6B", "ACCENT_CORAL_HOVER": "#FF8585", "ACCENT_CORAL_DEEP": "#C42B1C",
        "ACCENT_YELLOW": "#FCE100", "ACCENT_LAVENDER": "#7E8CE0", "ACCENT_LAVENDER_HOVER": "#93A0EC",
        "SHADOW": "#141414",
    },
}

DEFAULT_THEME_NAME = "Windows 11 Dark"


class Palette:
    pass


def apply_theme(theme_name, refresh_window=None):
    theme = THEMES.get(theme_name, THEMES[DEFAULT_THEME_NAME])
    for key, value in theme.items():
        setattr(Palette, key, value)
    global CURRENT_THEME_NAME
    CURRENT_THEME_NAME = theme_name
    if refresh_window is not None:
        refresh_window.refresh_theme()


CURRENT_THEME_NAME = DEFAULT_THEME_NAME
apply_theme(DEFAULT_THEME_NAME)

try:
    import psutil
    PSUTIL_AVAILABLE = True
except Exception:
    psutil = None
    PSUTIL_AVAILABLE = False


# ==============================================================================
# 2. FILE LOGGER
# ==============================================================================
class RinkaLogger:
    def __init__(self, log_dir):
        self.log_dir = log_dir
        self.lock = threading.Lock()
        self._current_date_str = None
        self._file = None
        try:
            os.makedirs(self.log_dir, exist_ok=True)
        except Exception as e:
            self._safe_print(f"[RinkaLogger] Gagal membuat folder log: {e}")

        self._queue = queue.Queue()
        self._worker = threading.Thread(target=self._worker_loop, daemon=True)
        self._worker.start()

    @staticmethod
    def _safe_print(line):
        try:
            if sys.stdout is not None:
                print(line)
        except Exception:
            pass

    def _worker_loop(self):
        while True:
            level, message = self._queue.get()
            self._write_now(level, message)

    def _ensure_file(self):
        today_str = datetime.date.today().strftime("%Y-%m-%d")
        if today_str == self._current_date_str and self._file is not None:
            return
        if self._file is not None:
            try:
                self._file.close()
            except Exception:
                pass
        self._current_date_str = today_str
        path = os.path.join(self.log_dir, f"{today_str}.txt")
        try:
            self._file = open(path, "a", encoding="utf-8")
        except Exception as e:
            self._safe_print(f"[RinkaLogger] Gagal membuka file log {path}: {e}")
            self._file = None

    def _write(self, level, message):
        self._queue.put((level, message))

    def _write_now(self, level, message):
        with self.lock:
            self._ensure_file()
            timestamp = datetime.datetime.now().strftime("%H:%M:%S")
            line = f"[{timestamp}] [{level:5s}] {message}"
            self._safe_print(line)
            if self._file is not None:
                try:
                    self._file.write(line + "\n")
                    self._file.flush()
                except Exception:
                    pass

    def info(self, message):
        self._write("INFO", message)

    def warning(self, message):
        self._write("WARN", message)

    def error(self, message):
        self._write("ERROR", message)


rinka_logger = RinkaLogger(LOG_DIR)


# ==============================================================================
# 3. AUDIO FIFO RING BUFFER DENGAN DRIFT COMPENSATION
# ==============================================================================

class AudioFIFO:
    def __init__(self, channels=2, max_samples=16384):
        self.channels = channels
        self.max_samples = max_samples
        self.buffer = np.zeros((channels, max_samples), dtype=np.float32)
        self.write_pos = 0
        self.read_pos = 0
        self.available = 0
        self.lock = threading.Lock()

    def reset(self):
        with self.lock:
            self.buffer.fill(0)
            self.write_pos = 0
            self.read_pos = 0
            self.available = 0

    def write(self, data, max_lead_samples=1024):
        with self.lock:
            n = data.shape[1]
            if n > self.max_samples:
                data = data[:, -self.max_samples:]
                n = self.max_samples

            if self.available > max_lead_samples:
                skip = self.available - max_lead_samples
                self.read_pos = (self.read_pos + skip) % self.max_samples
                self.available = max_lead_samples

            end_pos = self.write_pos + n
            if end_pos <= self.max_samples:
                self.buffer[:, self.write_pos:end_pos] = data
            else:
                first = self.max_samples - self.write_pos
                self.buffer[:, self.write_pos:] = data[:, :first]
                self.buffer[:, :n - first] = data[:, first:]

            self.write_pos = (self.write_pos + n) % self.max_samples
            self.available = min(self.max_samples, self.available + n)

    def read(self, n, target_channels=2):
        with self.lock:
            if self.available < n:
                out = np.zeros((self.channels, n), dtype=np.float32)
                if self.available > 0:
                    avail = self.available
                    end_pos = self.read_pos + avail
                    if end_pos <= self.max_samples:
                        out[:, :avail] = self.buffer[:, self.read_pos:end_pos]
                    else:
                        first = self.max_samples - self.read_pos
                        out[:, :first] = self.buffer[:, self.read_pos:]
                        out[:, first:avail] = self.buffer[:, :avail - first]
                    self.read_pos = (self.read_pos + avail) % self.max_samples
                    self.available = 0
                return out[:target_channels, :] if target_channels <= self.channels else np.repeat(out[0:1, :], target_channels, axis=0)

            out = np.empty((self.channels, n), dtype=np.float32)
            end_pos = self.read_pos + n
            if end_pos <= self.max_samples:
                out[:] = self.buffer[:, self.read_pos:end_pos]
            else:
                first = self.max_samples - self.read_pos
                out[:, :first] = self.buffer[:, self.read_pos:]
                out[:, first:] = self.buffer[:, :n - first]

            self.read_pos = (self.read_pos + n) % self.max_samples
            self.available -= n
            return out[:target_channels, :] if target_channels <= self.channels else np.repeat(out[0:1, :], target_channels, axis=0)


# ==============================================================================
# 3b. LATENCY PROBE
# ==============================================================================

class LatencyProbe:
    def __init__(self, seconds=1.8, sample_rate=48000):
        self.lock = threading.Lock()
        self.sample_rate = sample_rate
        self.max_samples = max(256, int(seconds * sample_rate))
        self.buffer = np.zeros(self.max_samples, dtype=np.float32)
        self.write_pos = 0
        self.filled = 0

    def set_sample_rate(self, sample_rate, seconds=1.8):
        with self.lock:
            self.sample_rate = sample_rate
            self.max_samples = max(256, int(seconds * sample_rate))
            self.buffer = np.zeros(self.max_samples, dtype=np.float32)
            self.write_pos = 0
            self.filled = 0

    def write(self, mono_block):
        with self.lock:
            n = mono_block.shape[0]
            if n <= 0:
                return
            if n >= self.max_samples:
                self.buffer[:] = mono_block[-self.max_samples:]
                self.write_pos = 0
                self.filled = self.max_samples
                return
            end = self.write_pos + n
            if end <= self.max_samples:
                self.buffer[self.write_pos:end] = mono_block
            else:
                first = self.max_samples - self.write_pos
                self.buffer[self.write_pos:] = mono_block[:first]
                self.buffer[:n - first] = mono_block[first:]
            self.write_pos = (self.write_pos + n) % self.max_samples
            self.filled = min(self.max_samples, self.filled + n)

    def snapshot(self):
        with self.lock:
            if self.filled < self.max_samples:
                return self.buffer[:self.write_pos].copy()
            return np.concatenate([self.buffer[self.write_pos:], self.buffer[:self.write_pos]])


def estimate_latency_ms(probe_a, probe_b, max_lag_ms=1500.0):
    if probe_a.sample_rate != probe_b.sample_rate:
        return None

    sample_rate = probe_a.sample_rate
    a = probe_a.snapshot()
    b = probe_b.snapshot()

    n = min(a.size, b.size)
    if n < int(0.2 * sample_rate):
        return None

    a = a[-n:]
    b = b[-n:]

    # Gate hening: hanya hitung jika kamu sedang bersuara di mic
    if float(np.sqrt(np.mean(a ** 2))) < 1e-3 or float(np.sqrt(np.mean(b ** 2))) < 1e-3:
        return None

    # ENVELOPE EXTRACTION (Khusus Voice Changer & AI RVC):
    # Downsample ke selubung energi 400 Hz untuk mendeteksi ritme silabel/suku kata bicara
    block_size = max(1, int(sample_rate / 400))
    trunc_len = (n // block_size) * block_size
    if trunc_len <= block_size:
        return None

    env_a = np.mean(np.abs(a[:trunc_len]).reshape(-1, block_size), axis=1)
    env_b = np.mean(np.abs(b[:trunc_len]).reshape(-1, block_size), axis=1)
    curr_sr = sample_rate / float(block_size)

    x = env_a - np.mean(env_a)
    y = env_b - np.mean(env_b)

    curr_n = min(x.size, y.size)
    max_lag = min(int((max_lag_ms / 1000.0) * curr_sr), curr_n - 1)
    if max_lag <= 0:
        return None

    fft_size = 1
    while fft_size < 2 * curr_n:
        fft_size *= 2

    fx = np.fft.rfft(x, fft_size)
    fy = np.fft.rfft(y, fft_size)
    corr = np.fft.irfft(fy * np.conj(fx), fft_size)
    corr = np.concatenate([corr[-max_lag:], corr[:max_lag + 1]])

    lag_samples = int(np.argmax(corr)) - max_lag
    return abs((lag_samples / curr_sr) * 1000.0)


# ==============================================================================
# 3c. AUDIO OVERLAY PLAYER & BANK
# ==============================================================================

class AudioOverlayPlayer:
    def __init__(self):
        self.lock = threading.Lock()
        self.enabled = False
        self.file_path = None
        self.gain = 0.5
        self.sample_rate = 48000
        self.data = None
        self.play_pos = 0

    def load_file(self, path, target_sample_rate):
        try:
            with AudioFile(path).resampled_to(target_sample_rate) as f:
                audio = f.read(f.frames)

            if audio.ndim == 1:
                audio = np.stack([audio, audio], axis=0)
            elif audio.shape[0] == 1:
                audio = np.repeat(audio, 2, axis=0)
            else:
                audio = audio[:2, :]
            audio = np.ascontiguousarray(audio.astype(np.float32))

            crossfade_ms = 80.0
            cf_n = min(int(target_sample_rate * crossfade_ms / 1000.0), audio.shape[1] // 4)
            if cf_n > 8:
                fade_out = np.linspace(1.0, 0.0, cf_n, dtype=np.float32)
                fade_in = np.linspace(0.0, 1.0, cf_n, dtype=np.float32)
                blended = audio[:, -cf_n:] * fade_out + audio[:, :cf_n] * fade_in
                audio = audio.copy()
                audio[:, :cf_n] = blended
                audio = audio[:, :-cf_n]

            duration_sec = audio.shape[1] / float(target_sample_rate)
            with self.lock:
                self.data = audio
                self.sample_rate = target_sample_rate
                self.play_pos = 0
                self.file_path = path
            return True, duration_sec
        except Exception as e:
            return False, str(e)

    def set_enabled(self, enabled):
        self.enabled = enabled

    def set_gain(self, gain_0_2):
        self.gain = max(0.0, min(2.0, gain_0_2))

    def mix_into(self, audio_stereo):
        if not self.enabled or self.data is None:
            return audio_stereo
        with self.lock:
            data = self.data
            total = data.shape[1]
            if total <= 0:
                return audio_stereo

            n = audio_stereo.shape[1]
            out = audio_stereo.copy()
            pos = self.play_pos
            written = 0
            while written < n:
                take = min(n - written, total - pos)
                out[:, written:written + take] += data[:, pos:pos + take] * self.gain
                pos += take
                written += take
                if pos >= total:
                    pos = 0
            self.play_pos = pos
            return out


class OverlayBank:
    MAX_SLOTS = 1

    def __init__(self):
        self.players = [AudioOverlayPlayer()]

    def add_slot(self):
        if len(self.players) < self.MAX_SLOTS:
            self.players.append(AudioOverlayPlayer())
            return True
        return False

    def remove_slot(self, index):
        if 0 <= index < len(self.players) and len(self.players) > 1:
            self.players.pop(index)
            return True
        return False

    def mix_into(self, audio_stereo):
        out = audio_stereo
        for player in self.players:
            out = player.mix_into(out)
        return out


def soft_limit(audio, threshold=0.9, ceiling=0.99):
    peak = np.max(np.abs(audio)) if audio.size > 0 else 0.0
    if peak <= threshold:
        return audio

    out = audio.copy()
    mask = np.abs(out) > threshold
    sign = np.sign(out[mask])
    excess = np.abs(out[mask]) - threshold
    headroom = ceiling - threshold
    out[mask] = sign * (threshold + headroom * np.tanh(excess / headroom))
    return out


# ==============================================================================
# 4. WASAPI DEVICE FILTER & WIN32 UTILITY
# ==============================================================================

def get_wasapi_devices():
    wasapi_hostapi_idx = None
    try:
        hostapis = sd.query_hostapis()
        for idx, api in enumerate(hostapis):
            if "wasapi" in api['name'].lower():
                wasapi_hostapi_idx = idx
                break
    except Exception as e:
        rinka_logger.error(f"Error query host APIs: {e}")

    try:
        devices = sd.query_devices()
    except Exception as e:
        rinka_logger.error(f"Error query devices: {e}")
        devices = []

    wasapi_inputs = []
    wasapi_outputs = []

    for idx, dev in enumerate(devices):
        if wasapi_hostapi_idx is not None and dev['hostapi'] != wasapi_hostapi_idx:
            continue
        dev_name = dev['name']
        if dev['max_input_channels'] > 0:
            wasapi_inputs.append((idx, dev_name, dev))
        if dev['max_output_channels'] > 0:
            wasapi_outputs.append((idx, dev_name, dev))

    return wasapi_inputs, wasapi_outputs


def _enum_process_windows(main_win_hwnd):
    if sys.platform != "win32":
        return set()
    current_pid = os.getpid()
    found = []

    def enum_proc(hwnd, lParam):
        if user32.IsWindowVisible(hwnd):
            pid = wintypes.DWORD()
            user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
            if pid.value == current_pid and hwnd != main_win_hwnd:
                found.append(hwnd)
        return True

    cb = WNDENUMPROC(enum_proc)
    user32.EnumWindows(cb, 0)
    return set(found)


def fix_vst_window_titlebar(vst_name, main_win_hwnd, known_hwnds_before=None):
    if sys.platform != "win32":
        return

    current = _enum_process_windows(main_win_hwnd)
    target_hwnds = (current - known_hwnds_before) if known_hwnds_before is not None else current

    WS_CAPTION = 0x00C00000
    WS_SYSMENU = 0x00080000
    WS_THICKFRAME = 0x00040000
    WS_MINIMIZEBOX = 0x00020000

    SWP_NOZORDER = 0x0004
    SWP_FRAMECHANGED = 0x0020
    SWP_SHOWWINDOW = 0x0040

    for hwnd in target_hwnds:
        style = user32.GetWindowLongW(hwnd, -16)
        new_style = style | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_THICKFRAME
        user32.SetWindowLongW(hwnd, -16, new_style)
        user32.SetWindowTextW(hwnd, f"🎛️ VST: {vst_name}")

        rect = wintypes.RECT()
        user32.GetWindowRect(hwnd, ctypes.byref(rect))
        w = max(400, rect.right - rect.left)
        h = max(300, rect.bottom - rect.top)

        if rect.left <= 10 and rect.top <= 10:
            screen_w = user32.GetSystemMetrics(0)
            screen_h = user32.GetSystemMetrics(1)
            x = max(50, (screen_w - w) // 2)
            y = max(50, (screen_h - h) // 2)
            user32.SetWindowPos(hwnd, 0, x, y, w, h, SWP_NOZORDER | SWP_FRAMECHANGED | SWP_SHOWWINDOW)
        else:
            user32.SetWindowPos(hwnd, 0, rect.left, rect.top, w, h, SWP_NOZORDER | SWP_FRAMECHANGED | SWP_SHOWWINDOW)


# ==============================================================================
# 5. VST SCANNER & QUICK PICKER
# ==============================================================================

class VSTScanner:
    @staticmethod
    def get_vst_directories():
        dirs = []
        common_pf = os.environ.get("CommonProgramFiles", "C:\\Program Files\\Common Files")
        dirs.append(os.path.join(common_pf, "VST3"))

        pf = os.environ.get("ProgramFiles", "C:\\Program Files")
        dirs.append(os.path.join(pf, "VSTPlugins"))
        dirs.append(os.path.join(pf, "Steinberg", "VSTPlugins"))

        return [d for d in dirs if os.path.exists(d)]

    @staticmethod
    def scan_plugins():
        vst_map = {}
        target_dirs = VSTScanner.get_vst_directories()

        for base_dir in target_dirs:
            for root, dirs_list, files in os.walk(base_dir):
                for d in list(dirs_list):
                    if d.lower().endswith(".vst3"):
                        full_dir = os.path.join(root, d)
                        name = os.path.splitext(d)[0]
                        vst_map[name] = full_dir

                for file in files:
                    if file.lower().endswith(".vst3"):
                        full_path = os.path.join(root, file)
                        name = os.path.splitext(file)[0]
                        vst_map[name] = full_path

        return dict(sorted(vst_map.items(), key=lambda x: x[0].lower()))


class VSTSelectorDialog(QDialog):
    def __init__(self, parent=None, scanned_vst=None):
        super().__init__(parent)
        self.setWindowTitle("🔍 Pilih Plugin VST3 - Rinka SoundBridge")
        self.resize(520, 560)
        self.setStyleSheet(f"""
            QDialog {{ background-color: {Palette.BG_APP}; }}
            QLabel {{ color: {Palette.TEXT_PRIMARY}; }}
            QLineEdit {{
                background-color: {Palette.BG_INPUT};
                color: {Palette.TEXT_PRIMARY};
                border: 1px solid {Palette.BORDER};
                border-radius: 8px;
                padding: 8px;
                font-size: 13px;
            }}
            QListWidget {{
                background-color: {Palette.BG_PANEL};
                color: {Palette.TEXT_PRIMARY};
                border: 1px solid {Palette.BORDER};
                border-radius: 8px;
                padding: 4px;
                font-size: 13px;
            }}
            QListWidget::item {{
                padding: 8px;
                border-bottom: 1px solid {Palette.BORDER};
                border-radius: 6px;
            }}
            QListWidget::item:hover {{ background-color: {Palette.ACCENT_PINK_SOFT}; }}
            QListWidget::item:selected {{ background-color: {Palette.ACCENT_LAVENDER}; color: {Palette.TEXT_PRIMARY}; }}
        """)

        self.scanned_vst = scanned_vst or {}
        self.selected_path = None
        self.init_ui()

    def init_ui(self):
        layout = QVBoxLayout(self)
        layout.setContentsMargins(16, 16, 16, 16)
        layout.setSpacing(10)

        lbl_search = QLabel("🔎 Cari Plugin VST3:")
        lbl_search.setFont(QFont("Segoe UI", 10, QFont.Weight.Bold))
        layout.addWidget(lbl_search)

        self.txt_search = QLineEdit()
        self.txt_search.setPlaceholderText("Ketik nama VST...")
        self.txt_search.textChanged.connect(self.filter_list)
        layout.addWidget(self.txt_search)

        self.list_widget = QListWidget()
        self.list_widget.setSelectionMode(QAbstractItemView.SelectionMode.SingleSelection)
        self.list_widget.itemDoubleClicked.connect(self.on_item_double_clicked)
        layout.addWidget(self.list_widget, stretch=1)

        btn_layout = QHBoxLayout()
        btn_layout.setSpacing(8)

        self.btn_rescan = QPushButton("🔄 Scan Ulang")
        self.btn_rescan.setStyleSheet(f"background: {Palette.ACCENT_BLUE}; color: {Palette.TEXT_PRIMARY}; padding: 8px 12px; border-radius: 8px; font-weight: bold;")
        self.btn_rescan.clicked.connect(self.rescan_plugins)
        btn_layout.addWidget(self.btn_rescan)

        self.btn_manual = QPushButton("📁 Cari Manual (.vst3)...")
        self.btn_manual.setStyleSheet(f"background: {Palette.ACCENT_GREEN}; color: {Palette.TEXT_PRIMARY}; padding: 8px 12px; border-radius: 8px; font-weight: bold;")
        self.btn_manual.clicked.connect(self.browse_manual)
        btn_layout.addWidget(self.btn_manual)

        btn_layout.addStretch()

        self.btn_ok = QPushButton("✔ Pilih VST")
        self.btn_ok.setStyleSheet(f"background: {Palette.ACCENT_PINK}; color: {Palette.TEXT_PRIMARY}; padding: 8px 16px; border-radius: 8px; font-weight: bold;")
        self.btn_ok.clicked.connect(self.on_accept_selected)
        btn_layout.addWidget(self.btn_ok)

        layout.addLayout(btn_layout)
        self.populate_list(self.scanned_vst)

    def populate_list(self, vst_map):
        self.list_widget.clear()
        for name, path in vst_map.items():
            item = QListWidgetItem(f"🧩 {name}")
            item.setToolTip(path)
            item.setData(Qt.ItemDataRole.UserRole, path)
            self.list_widget.addItem(item)

        if self.list_widget.count() > 0:
            self.list_widget.setCurrentRow(0)

    def filter_list(self, text):
        query = text.lower().strip()
        filtered = {k: v for k, v in self.scanned_vst.items() if query in k.lower()}
        self.populate_list(filtered)

    def rescan_plugins(self):
        self.btn_rescan.setText("⏳ Memindai...")
        self.btn_rescan.setEnabled(False)
        QApplication.processEvents()

        self.scanned_vst = VSTScanner.scan_plugins()
        self.filter_list(self.txt_search.text())

        self.btn_rescan.setText("🔄 Scan Ulang")
        self.btn_rescan.setEnabled(True)

    def browse_manual(self):
        file_path, _ = QFileDialog.getOpenFileName(
            self, "Pilih File Plugin VST3 Manual", "", "VST3 Plugins (*.vst3)"
        )
        if file_path:
            self.selected_path = file_path
            self.accept()

    def on_accept_selected(self):
        item = self.list_widget.currentItem()
        if item:
            self.selected_path = item.data(Qt.ItemDataRole.UserRole)
            self.accept()

    def on_item_double_clicked(self, item):
        if item:
            self.selected_path = item.data(Qt.ItemDataRole.UserRole)
            self.accept()


# ==============================================================================
# 6. AUDIO PIPELINE
# ==============================================================================

def capture_vst_parameters(plugin):
    params = {}
    try:
        for pname, param in plugin.parameters.items():
            try:
                params[pname] = float(param.raw_value)
            except Exception:
                pass
    except Exception as e:
        rinka_logger.warning(f"Gagal membaca parameter VST: {e}")
    return params


def restore_vst_parameters(plugin, params_dict):
    if not params_dict:
        return
    try:
        plugin_params = plugin.parameters
        for pname, raw_val in params_dict.items():
            if pname in plugin_params:
                try:
                    plugin_params[pname].raw_value = float(raw_val)
                except Exception as pe:
                    rinka_logger.warning(f"Gagal restore parameter '{pname}': {pe}")
    except Exception as e:
        rinka_logger.warning(f"Gagal membaca parameter VST saat restore: {e}")


class VSTSlot:
    def __init__(self, file_path, name, plugin_instance):
        self.file_path = file_path
        self.name = name
        self.plugin = plugin_instance
        self.bypassed = False
        self.latency_buffer = None
        self.error_count = 0
        self.last_error_message = None
        self.last_error_time = 0.0
        self.cached_params = capture_vst_parameters(plugin_instance) if plugin_instance else {}

    def record_error(self, message):
        self.error_count += 1
        self.last_error_message = message
        self.last_error_time = time.time()

    def clear_error(self):
        self.error_count = 0
        self.last_error_message = None
        self.last_error_time = 0.0


class AudioPipeline:
    def __init__(self, name="Line"):
        self.name = name
        self.vst_slots = []
        self.lock = threading.Lock()

        self.is_enabled = True
        self.input_device = None
        self.output_device = None
        self.sample_rate = 48000
        self.buffer_size = 256
        self.master_gain = 1.0
        self.is_muted = False

        self.in_channels = 2
        self.out_channels = 2

        self.in_stream = None
        self.out_stream = None
        self.fifo = AudioFIFO(channels=2, max_samples=16384)

        self.peak_level = 0.0
        self.status_message = "Siap"

        self.probe_in = LatencyProbe(seconds=1.8, sample_rate=self.sample_rate)
        self.probe_out = LatencyProbe(seconds=1.8, sample_rate=self.sample_rate)

        self.overlay_input_bank = OverlayBank()
        self.overlay_output_bank = OverlayBank()

    def set_enabled(self, enabled):
        self.is_enabled = enabled
        if not enabled:
            self.stop_stream()
            self.peak_level = 0.0
            self.status_message = "⚪ Line Dinonaktifkan (OFF)"
        else:
            self.restart_stream()

    def update_devices(self, in_dev, out_dev, sample_rate, buffer_size):
        self.input_device = in_dev
        self.output_device = out_dev
        self.sample_rate = sample_rate
        self.buffer_size = buffer_size
        if self.is_enabled:
            self.restart_stream()
        else:
            self.stop_stream()
            self.status_message = "⚪ Line Dinonaktifkan (OFF)"

    def _open_stream_with_retry(self, open_fn, samplerate, stream_label,
                                 max_retries=4, initial_delay=0.15):
        delay = initial_delay
        last_err = None
        for attempt in range(1, max_retries + 1):
            try:
                return open_fn(samplerate)
            except Exception as e:
                last_err = e
                err_str = str(e).lower()
                transient = any(kw in err_str for kw in [
                    "busy", "in use", "unanticipated", "device unavailable",
                    "access is denied", "-9985", "-9998", "resource",
                ])
                if attempt >= max_retries or not transient:
                    raise
                self.status_message = f"⏳ {stream_label} sibuk, percobaan {attempt}/{max_retries}..."
                rinka_logger.warning(
                    f"[{self.name}] Buka {stream_label} gagal (percobaan {attempt}/{max_retries}): "
                    f"{e} - retry dalam {delay:.2f}s"
                )
                time.sleep(delay)
                delay *= 2
        raise last_err

    def restart_stream(self):
        self.stop_stream()
        if not self.is_enabled:
            self.status_message = "⚪ Line Dinonaktifkan (OFF)"
            return

        if self.input_device is None or self.output_device is None:
            self.status_message = "Pilih Input & Output WASAPI"
            return

        self.fifo.reset()

        with self.lock:
            for slot in self.vst_slots:
                slot.latency_buffer = None
                if slot.plugin is not None:
                    try:
                        slot.plugin.reset()
                    except Exception:
                        pass

        self.probe_in.set_sample_rate(self.sample_rate, seconds=1.8)
        self.probe_out.set_sample_rate(self.sample_rate, seconds=1.8)

        try:
            in_info = sd.query_devices(self.input_device)
            out_info = sd.query_devices(self.output_device)

            self.in_channels = min(2, max(1, in_info['max_input_channels']))
            self.out_channels = min(2, max(1, out_info['max_output_channels']))

            in_extra = None
            out_extra = None
            if sys.platform == "win32":
                in_extra = sd.WasapiSettings(exclusive=False, auto_convert=True)
                out_extra = sd.WasapiSettings(exclusive=False, auto_convert=True)

            def _open_input(samplerate):
                return sd.InputStream(
                    device=self.input_device,
                    samplerate=samplerate,
                    blocksize=self.buffer_size,
                    dtype='float32',
                    channels=self.in_channels,
                    callback=self._in_callback,
                    extra_settings=in_extra
                )

            def _open_output(samplerate):
                return sd.OutputStream(
                    device=self.output_device,
                    samplerate=samplerate,
                    blocksize=self.buffer_size,
                    dtype='float32',
                    channels=self.out_channels,
                    callback=self._out_callback,
                    extra_settings=out_extra
                )

            try:
                self.in_stream = self._open_stream_with_retry(_open_input, self.sample_rate, "Input")
                self.out_stream = self._open_stream_with_retry(_open_output, self.sample_rate, "Output")
            except Exception as open_err:
                if self.in_stream is not None:
                    try:
                        self.in_stream.close()
                    except Exception:
                        pass
                    self.in_stream = None

                fallback_sr = int(round(
                    out_info.get('default_samplerate')
                    or in_info.get('default_samplerate')
                    or self.sample_rate
                ))
                if fallback_sr == self.sample_rate:
                    raise

                rinka_logger.warning(f"[{self.name}] Sample rate {self.sample_rate}Hz ditolak ({open_err}). Fallback ke {fallback_sr}Hz...")
                self.sample_rate = fallback_sr
                self.in_stream = self._open_stream_with_retry(_open_input, self.sample_rate, "Input")
                self.out_stream = self._open_stream_with_retry(_open_output, self.sample_rate, "Output")

            self.in_stream.start()
            self.out_stream.start()
            self.probe_in.set_sample_rate(self.sample_rate, seconds=1.8)
            self.probe_out.set_sample_rate(self.sample_rate, seconds=1.8)
            self.status_message = f"🟢 WASAPI Aktif ({self.in_channels} In ➔ {self.out_channels} Out @ {self.sample_rate}Hz)"
        except Exception as e:
            self.status_message = f"⚠️ Error: {str(e)[:38]}"
            rinka_logger.error(f"[{self.name}] WASAPI Pipeline Error: {e}")

    def stop_stream(self):
        if self.in_stream is not None:
            try:
                self.in_stream.stop()
                self.in_stream.close()
            except Exception:
                pass
            self.in_stream = None

        if self.out_stream is not None:
            try:
                self.out_stream.stop()
                self.out_stream.close()
            except Exception:
                pass
            self.out_stream = None

        with self.lock:
            for slot in self.vst_slots:
                slot.latency_buffer = None
        self.fifo.reset()

    def _in_callback(self, indata, frames, time_info, status):
        if not self.is_enabled:
            return

        if self.in_channels == 1:
            audio_stereo = np.repeat(indata.T, 2, axis=0)
        else:
            audio_stereo = indata.T[:2, :].copy()

        frames = audio_stereo.shape[1]
        self.probe_in.write(audio_stereo.mean(axis=0))

        audio_stereo = self.overlay_input_bank.mix_into(audio_stereo)
        audio_stereo = soft_limit(audio_stereo)

        with self.lock:
            for slot in self.vst_slots:
                if not slot.bypassed and slot.plugin is not None:
                    try:
                        processed = slot.plugin(audio_stereo, self.sample_rate, reset=False)

                        if processed.ndim == 1:
                            processed = np.stack([processed, processed], axis=0)
                        elif processed.shape[0] == 1:
                            processed = np.repeat(processed, 2, axis=0)
                        else:
                            processed = processed[:2, :]

                        if slot.latency_buffer is not None and slot.latency_buffer.shape[1] > 0:
                            processed = np.concatenate([slot.latency_buffer, processed], axis=1)
                            slot.latency_buffer = None

                        if processed.shape[1] > frames:
                            slot.latency_buffer = processed[:, frames:].copy()
                            audio_stereo = processed[:, :frames]
                        elif processed.shape[1] == frames:
                            audio_stereo = processed
                        else:
                            pad = np.zeros((2, frames - processed.shape[1]), dtype=np.float32)
                            audio_stereo = np.concatenate([processed, pad], axis=1)
                    except Exception as fx_err:
                        slot.record_error(str(fx_err))
                        err_msg = str(fx_err)
                        now = time.time()
                        if (slot.error_count == 1
                                or err_msg != getattr(slot, "_last_logged_error", None)
                                or now - getattr(slot, "_last_logged_time", 0.0) > 5.0):
                            rinka_logger.error(
                                f"[{self.name}] VST '{slot.name}' gagal diproses "
                                f"(ke-{slot.error_count}x): {err_msg}"
                            )
                            slot._last_logged_error = err_msg
                            slot._last_logged_time = now

        audio_stereo = soft_limit(audio_stereo)
        self.fifo.write(audio_stereo, max_lead_samples=max(512, self.buffer_size * 4))

    def _out_callback(self, outdata, frames, time_info, status):
        if not self.is_enabled:
            outdata.fill(0)
            self.peak_level = 0.0
            return

        audio_stereo = self.fifo.read(frames, target_channels=2)

        if self.is_muted:
            final_audio = np.zeros_like(audio_stereo)
        else:
            final_audio = (audio_stereo * self.master_gain).astype(np.float32)
            final_audio = self.overlay_output_bank.mix_into(final_audio)
            final_audio = soft_limit(final_audio)

        self.probe_out.write(final_audio.mean(axis=0))

        peak = float(np.max(np.abs(final_audio))) if final_audio.size > 0 else 0.0
        self.peak_level = min(peak, 1.5)

        if self.out_channels == 1:
            outdata[:] = final_audio[0:1, :].T
        else:
            outdata[:] = final_audio[:self.out_channels, :].T

    def add_vst(self, file_path):
        try:
            plugin = load_plugin(file_path)
            name = os.path.splitext(os.path.basename(file_path))[0]
            slot = VSTSlot(file_path, name, plugin)
            with self.lock:
                self.vst_slots.append(slot)
            return slot
        except Exception as e:
            raise RuntimeError(f"Gagal memuat VST: {str(e)}")

    def remove_vst(self, index):
        with self.lock:
            if 0 <= index < len(self.vst_slots):
                removed = self.vst_slots.pop(index)
                if removed.plugin is not None:
                    try:
                        removed.plugin.reset()
                    except Exception:
                        pass
                removed.latency_buffer = None

    def move_vst(self, index, direction):
        with self.lock:
            new_idx = index + direction
            if 0 <= new_idx < len(self.vst_slots):
                self.vst_slots[index], self.vst_slots[new_idx] = (
                    self.vst_slots[new_idx],
                    self.vst_slots[index],
                )
                self.vst_slots[index].latency_buffer = None
                self.vst_slots[new_idx].latency_buffer = None


# ==============================================================================
# 7. UI COMPONENTS & CARD WIDGETS
# ==============================================================================

class ElidedLabel(QLabel):
    def __init__(self, text="", parent=None):
        super().__init__(parent)
        self._full_text = text
        self.setFont(QFont("Segoe UI", 9, QFont.Weight.Bold))
        self.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Preferred)
        self.setMinimumWidth(40)
        self.setToolTip(text)

    def setText(self, text):
        self._full_text = text
        self.setToolTip(text)
        self.update()

    def paintEvent(self, event):
        painter = QPainter(self)
        painter.setFont(self.font())
        metrics = painter.fontMetrics()
        elided = metrics.elidedText(self._full_text, Qt.TextElideMode.ElideRight, self.width() - 4)
        painter.setPen(QColor(Palette.TEXT_PRIMARY))
        painter.drawText(self.rect(), Qt.AlignmentFlag.AlignVCenter | Qt.AlignmentFlag.AlignLeft, elided)


class ClickableElidedLabel(QFrame):
    clicked = pyqtSignal()

    def __init__(self, text="", parent=None):
        super().__init__(parent)
        self._full_text = text
        self.setCursor(Qt.CursorShape.PointingHandCursor)
        self.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Preferred)
        self.setMinimumWidth(40)
        self.setFrameShape(QFrame.Shape.StyledPanel)
        self.refresh_theme()

    def setText(self, text):
        self._full_text = text
        self.update()

    def refresh_theme(self):
        self.setStyleSheet(f"""
            QFrame {{
                background-color: {Palette.BG_INPUT};
                border: 1px solid {Palette.BORDER};
                border-radius: 6px;
            }}
            QFrame:hover {{
                background-color: {Palette.ACCENT_PINK_SOFT};
                border: 1px solid {Palette.ACCENT_PINK};
            }}
        """)
        self.update()

    def mousePressEvent(self, event):
        if event.button() == Qt.MouseButton.LeftButton:
            self.clicked.emit()
        super().mousePressEvent(event)

    def paintEvent(self, event):
        super().paintEvent(event)
        painter = QPainter(self)
        painter.setFont(QFont("Segoe UI", 9, QFont.Weight.Bold))
        metrics = painter.fontMetrics()
        rect = self.contentsRect().adjusted(6, 0, -6, 0)
        elided = metrics.elidedText(self._full_text, Qt.TextElideMode.ElideRight, rect.width())
        painter.setPen(QColor(Palette.TEXT_PRIMARY))
        painter.drawText(rect, Qt.AlignmentFlag.AlignVCenter | Qt.AlignmentFlag.AlignLeft, elided)


class VUMeterWidget(QFrame):
    def __init__(self, parent=None):
        super().__init__(parent)
        self.setFixedHeight(18)
        self.level = 0.0

    def set_level(self, level):
        self.level = max(0.0, min(level, 1.0))
        self.update()

    def _get_gradient_color(self, t):
        # Gradasi: Hijau (0-60%) -> Kuning Amber (60-82%) -> Merah Peak (>82%)
        if t < 0.60:
            rel = t / 0.60
            r = int(35 + (175 - 35) * rel)
            g = int(210 + (225 - 210) * rel)
            b = int(90 - 50 * rel)
        elif t < 0.82:
            rel = (t - 0.60) / 0.22
            r = int(175 + (255 - 175) * rel)
            g = int(225 - (95 * rel))
            b = int(40 - 20 * rel)
        else:
            rel = min(1.0, (t - 0.82) / 0.18)
            r = 255
            g = int(130 - (105 * rel))
            b = int(20 + (25 * rel))
        return QColor(max(0, min(255, r)), max(0, min(255, g)), max(0, min(255, b)))

    def paintEvent(self, event):
        try:
            from PyQt6.QtCore import QPointF
            from PyQt6.QtGui import QPolygonF, QBrush, QPen

            w = self.width()
            h = self.height()
            if w <= 10 or h <= 5:
                return

            painter = QPainter(self)
            painter.setRenderHint(QPainter.RenderHint.Antialiasing)

            # 1. Background Frame Wadah Meter (Menggunakan integer bulat anti-crash)
            bg_color = QColor(getattr(Palette, "BG_INPUT", "#1E1E1E"))
            border_color = QColor(getattr(Palette, "BORDER", "#383838"))
            painter.setPen(QPen(border_color, 1))
            painter.setBrush(QBrush(bg_color))
            painter.drawRoundedRect(0, 0, int(w - 1), int(h - 1), 4, 4)

            # 2. Dimensi Balok Diagonal
            margin_x = 6.0
            margin_y = 3.0
            seg_w = 6.5       # Lebar balok
            gap = 2.5         # Jarak celah antar balok
            slant = 4.5       # Kemiringan diagonal (/)

            usable_w = float(w) - (2.0 * margin_x) - slant
            if usable_w <= 0:
                painter.end()
                return

            num_bars = max(8, int(usable_w / (seg_w + gap)))
            actual_step = usable_w / float(num_bars)

            # 3. Render Segmen Balok LED Diagonal
            for i in range(num_bars):
                t = (i + 1) / float(num_bars)
                x_bot = margin_x + (float(i) * actual_step)
                y_bot = float(h) - margin_y
                y_top = margin_y

                poly = QPolygonF([
                    QPointF(x_bot + slant, y_top),
                    QPointF(x_bot + slant + seg_w, y_top),
                    QPointF(x_bot + seg_w, y_bot),
                    QPointF(x_bot, y_bot)
                ])

                base_color = self._get_gradient_color(t)

                if t <= self.level:
                    bar_color = base_color
                else:
                    # Efek LED mati: redup dan transparan
                    bar_color = QColor(base_color.red(), base_color.green(), base_color.blue(), 35)

                painter.setPen(Qt.PenStyle.NoPen)
                painter.setBrush(QBrush(bar_color))
                painter.drawPolygon(poly)

            painter.end()
        except Exception:
            pass


def combobox_style():
    return f"""
    QComboBox {{
        background: {Palette.BG_INPUT};
        color: {Palette.TEXT_PRIMARY};
        border: 1px solid {Palette.BORDER};
        padding: 5px;
        border-radius: 6px;
    }}
    QComboBox:hover {{ border: 1px solid {Palette.ACCENT_PINK}; }}
    QComboBox::drop-down {{ border: none; width: 20px; }}
    QComboBox QAbstractItemView {{
        background: {Palette.BG_INPUT};
        color: {Palette.TEXT_PRIMARY};
        selection-background-color: {Palette.ACCENT_PINK_SOFT};
        border: 1px solid {Palette.BORDER};
    }}
"""


def groupbox_style():
    return f"""
    QGroupBox {{
        color: {Palette.TEXT_SECONDARY};
        font-weight: bold;
        font-size: 9.5pt;
        border: 1px solid {Palette.BORDER};
        border-radius: 10px;
        margin-top: 9px;
        padding-top: 8px;
        background-color: {Palette.BG_PANEL_ALT};
    }}
    QGroupBox::title {{
        subcontrol-origin: margin;
        left: 8px;
        padding: 0 4px;
        color: {Palette.ACCENT_PINK};
    }}
"""


class CompactStepper(QWidget):
    valueChanged = pyqtSignal(int)

    def __init__(self, minimum=0, maximum=200, value=100, step=1, suffix="%",
                 color=None, color_key="ACCENT_PINK", parent=None):
        super().__init__(parent)
        self._min = minimum
        self._max = maximum
        self._step = step
        self._value = max(minimum, min(maximum, value))
        self._suffix = suffix
        self._color_key = color_key
        self._color = color or getattr(Palette, color_key, Palette.ACCENT_PINK)

        layout = QHBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(3)

        self.btn_down = QPushButton("◀")
        self.btn_down.setFixedSize(20, 20)
        layout.addWidget(self.btn_down)

        self.lbl_value = QLabel(f"{self._value}{suffix}")
        self.lbl_value.setFixedWidth(42)
        self.lbl_value.setAlignment(Qt.AlignmentFlag.AlignCenter)
        layout.addWidget(self.lbl_value)

        self.btn_up = QPushButton("▶")
        self.btn_up.setFixedSize(20, 20)
        layout.addWidget(self.btn_up)

        self.btn_down.clicked.connect(lambda: self._step_value(-self._step))
        self.btn_up.clicked.connect(lambda: self._step_value(self._step))
        self.refresh_theme()

    def refresh_theme(self):
        self._color = getattr(Palette, self._color_key, Palette.ACCENT_PINK)
        btn_style = f"""
            QPushButton {{ background: {Palette.BG_INPUT}; color: {Palette.TEXT_PRIMARY};
                border: 1px solid {Palette.BORDER}; border-radius: 5px; font-size: 8pt; }}
            QPushButton:hover {{ background: {Palette.ACCENT_PINK_SOFT}; }}
            QPushButton:disabled {{ color: {Palette.TEXT_MUTED}; }}
        """
        self.btn_down.setStyleSheet(btn_style)
        self.btn_up.setStyleSheet(btn_style)
        enabled = self.isEnabled()
        self.lbl_value.setStyleSheet(f"color: {self._color if enabled else Palette.TEXT_MUTED}; font-weight: bold;")

    def _step_value(self, delta):
        self.setValue(self._value + delta)

    def value(self):
        return self._value

    def setValue(self, v, emit=True):
        v = max(self._min, min(self._max, int(v)))
        changed = (v != self._value)
        self._value = v
        self.lbl_value.setText(f"{v}{self._suffix}")
        if changed and emit:
            self.valueChanged.emit(v)

    def setEnabled(self, enabled):
        super().setEnabled(enabled)
        self.btn_down.setEnabled(enabled)
        self.btn_up.setEnabled(enabled)
        self.lbl_value.setStyleSheet(f"color: {self._color if enabled else Palette.TEXT_MUTED}; font-weight: bold;")


# ------------------------------------------------------------------------------
# 7c. Kartu VST
# ------------------------------------------------------------------------------

class VSTCardWidget(QFrame):
    def __init__(self, slot, index, on_toggle_bypass, on_edit, on_move_up, on_move_down, on_delete):
        super().__init__()
        self.slot = slot
        self.index = index
        self.on_toggle_bypass = on_toggle_bypass

        self.setFrameShape(QFrame.Shape.StyledPanel)
        self.setStyleSheet(f"""
            QFrame {{
                background-color: {Palette.BG_PANEL};
                border: 1px solid {Palette.BORDER};
                border-radius: 8px;
            }}
        """)

        layout = QHBoxLayout(self)
        layout.setContentsMargins(6, 4, 6, 4)
        layout.setSpacing(5)

        self.lbl_title = ClickableElidedLabel(f"[{index + 1}] {slot.name}")
        self.lbl_title.setFixedHeight(28)
        self.lbl_title.setToolTip(f"🎛️ Klik untuk membuka editor GUI: {slot.name}\n📁 {slot.file_path}")
        self.lbl_title.clicked.connect(lambda: on_edit(self.slot))
        layout.addWidget(self.lbl_title, stretch=1)

        self.lbl_error_dot = QLabel("🔴")
        self.lbl_error_dot.setFixedWidth(18)
        self.lbl_error_dot.setToolTip("")
        self.lbl_error_dot.setVisible(False)
        layout.addWidget(self.lbl_error_dot)

        self.btn_bypass = QPushButton()
        self.btn_bypass.setFont(QFont("Segoe UI", 9, QFont.Weight.Bold))
        self.btn_bypass.setFixedWidth(80)
        self.btn_bypass.clicked.connect(lambda: self.on_toggle_bypass(self.index, self))
        layout.addWidget(self.btn_bypass)
        self.update_bypass_visual()

        self.btn_up = QPushButton("▲")
        self.btn_up.setFixedSize(24, 24)
        self.btn_up.setStyleSheet(f"background-color: {Palette.BG_PANEL_ALT}; color: {Palette.TEXT_PRIMARY}; border: 1px solid {Palette.BORDER}; border-radius: 5px; font-size: 10px;")
        self.btn_up.clicked.connect(lambda: on_move_up(self.index))
        layout.addWidget(self.btn_up)

        self.btn_down = QPushButton("▼")
        self.btn_down.setFixedSize(24, 24)
        self.btn_down.setStyleSheet(f"background-color: {Palette.BG_PANEL_ALT}; color: {Palette.TEXT_PRIMARY}; border: 1px solid {Palette.BORDER}; border-radius: 5px; font-size: 10px;")
        self.btn_down.clicked.connect(lambda: on_move_down(self.index))
        layout.addWidget(self.btn_down)

        self.btn_del = QPushButton("✕")
        self.btn_del.setFixedSize(24, 24)
        self.btn_del.setStyleSheet(f"""
            QPushButton {{
                background-color: {Palette.ACCENT_CORAL};
                color: #FFFFFF;
                border: none;
                border-radius: 5px;
                font-size: 10px;
            }}
            QPushButton:hover {{ background-color: {Palette.ACCENT_CORAL_HOVER}; }}
        """)
        self.btn_del.clicked.connect(lambda: on_delete(self.index))
        layout.addWidget(self.btn_del)

    def refresh_theme(self):
        self.setStyleSheet(f"""
            QFrame {{
                background-color: {Palette.BG_PANEL};
                border: 1px solid {Palette.BORDER};
                border-radius: 8px;
            }}
        """)
        self.lbl_title.refresh_theme()
        self.btn_up.setStyleSheet(f"background-color: {Palette.BG_PANEL_ALT}; color: {Palette.TEXT_PRIMARY}; border: 1px solid {Palette.BORDER}; border-radius: 5px; font-size: 10px;")
        self.btn_down.setStyleSheet(f"background-color: {Palette.BG_PANEL_ALT}; color: {Palette.TEXT_PRIMARY}; border: 1px solid {Palette.BORDER}; border-radius: 5px; font-size: 10px;")
        self.btn_del.setStyleSheet(f"""
            QPushButton {{
                background-color: {Palette.ACCENT_CORAL};
                color: #FFFFFF;
                border: none;
                border-radius: 5px;
                font-size: 10px;
            }}
            QPushButton:hover {{ background-color: {Palette.ACCENT_CORAL_HOVER}; }}
        """)
        self.update_bypass_visual()
        self.update()

    def update_bypass_visual(self):
        if self.slot.bypassed:
            self.btn_bypass.setText("💤 BYPASS")
            self.btn_bypass.setStyleSheet(f"""
                QPushButton {{
                    background-color: {Palette.ACCENT_CORAL};
                    color: #FFFFFF;
                    border: none;
                    border-radius: 5px;
                    padding: 4px;
                    font-size: 10px;
                }}
                QPushButton:hover {{ background-color: {Palette.ACCENT_CORAL_HOVER}; }}
            """)
        else:
            self.btn_bypass.setText("🌿 ON")
            self.btn_bypass.setStyleSheet(f"""
                QPushButton {{
                    background-color: {Palette.ACCENT_GREEN};
                    color: {Palette.TEXT_PRIMARY};
                    border: none;
                    border-radius: 5px;
                    padding: 4px;
                    font-size: 10px;
                }}
                QPushButton:hover {{ background-color: {Palette.ACCENT_GREEN_HOVER}; }}
            """)

    ERROR_INDICATOR_TIMEOUT_SEC = 4.0

    def refresh_error_indicator(self):
        slot = self.slot
        if slot.error_count > 0 and (time.time() - slot.last_error_time) < self.ERROR_INDICATOR_TIMEOUT_SEC:
            if not self.lbl_error_dot.isVisible():
                self.lbl_error_dot.setVisible(True)
            tooltip = (
                f"VST ini gagal diproses ({slot.error_count}x sejauh ini)\n"
                f"Error terakhir: {slot.last_error_message}"
            )
            if self.lbl_error_dot.toolTip() != tooltip:
                self.lbl_error_dot.setToolTip(tooltip)
        else:
            if self.lbl_error_dot.isVisible():
                self.lbl_error_dot.setVisible(False)


# ------------------------------------------------------------------------------
# 7d. Baris Slot Overlay Audio
# ------------------------------------------------------------------------------

class OverlaySlotRow(QFrame):
    def __init__(self, player, index, on_toggle, on_browse, on_gain_changed, on_remove, can_remove):
        super().__init__()
        self.player = player
        self.index = index

        layout = QHBoxLayout(self)
        layout.setContentsMargins(4, 2, 4, 2)
        layout.setSpacing(4)

        self.chk = QCheckBox()
        self.chk.setChecked(player.enabled)
        self.chk.setFixedWidth(18)
        self.chk.stateChanged.connect(lambda state: on_toggle(self.index))
        layout.addWidget(self.chk)

        self.btn_browse = QPushButton("📁")
        self.btn_browse.setFixedSize(22, 20)
        self.btn_browse.clicked.connect(lambda checked=False: on_browse(self.index))
        layout.addWidget(self.btn_browse)

        file_label = "(belum ada file)"
        if player.file_path:
            file_label = os.path.basename(player.file_path)
        self.lbl_file = ElidedLabel(file_label)
        self.lbl_file.setStyleSheet(f"color: {Palette.TEXT_MUTED}; font-size: 8pt; font-weight: normal;")
        layout.addWidget(self.lbl_file, stretch=1)

        self.stepper = CompactStepper(
            minimum=0, maximum=200, value=int(round(player.gain * 100)),
            step=1, color_key="ACCENT_BLUE_DEEP"
        )
        self.stepper.valueChanged.connect(lambda v: on_gain_changed(self.index, v))
        layout.addWidget(self.stepper)

        self.btn_remove = QPushButton("✕")
        self.btn_remove.setFixedSize(20, 20)
        self.btn_remove.setEnabled(can_remove)
        self.btn_remove.setVisible(OverlayBank.MAX_SLOTS > 1)
        self.btn_remove.setToolTip("Hapus slot ini" if can_remove else "Minimal 1 slot harus ada")
        self.btn_remove.clicked.connect(lambda checked=False: on_remove(self.index))
        layout.addWidget(self.btn_remove)

        self.refresh_theme()

    def refresh_theme(self):
        self.setStyleSheet(f"""
            QFrame {{
                background-color: {Palette.BG_PANEL};
                border: 1px solid {Palette.BORDER};
                border-radius: 6px;
            }}
        """)
        self.btn_browse.setStyleSheet(f"background: {Palette.BG_INPUT}; color: {Palette.TEXT_PRIMARY}; border: 1px solid {Palette.BORDER}; border-radius: 4px; font-size: 8pt;")
        self.lbl_file.setStyleSheet(f"color: {Palette.TEXT_MUTED}; font-size: 8pt; font-weight: normal;")
        self.lbl_file.update()
        self.btn_remove.setStyleSheet(f"""
            QPushButton {{
                background-color: {Palette.ACCENT_CORAL};
                color: #FFFFFF;
                border: none;
                border-radius: 4px;
                font-size: 8pt;
            }}
            QPushButton:hover {{ background-color: {Palette.ACCENT_CORAL_HOVER}; }}
            QPushButton:disabled {{ background-color: {Palette.BORDER}; color: {Palette.TEXT_MUTED}; }}
        """)
        self.stepper.refresh_theme()


# ------------------------------------------------------------------------------
# 7e. Panel Line (A atau B)
# ------------------------------------------------------------------------------

class LinePanelWidget(QFrame):
    def __init__(self, title, pipeline, wasapi_inputs, wasapi_outputs, main_window):
        super().__init__()
        self.title = title
        self.pipeline = pipeline
        self.wasapi_inputs = wasapi_inputs
        self.wasapi_outputs = wasapi_outputs
        self.main_window = main_window
        self._is_editing_vst = False

        self.setFrameShape(QFrame.Shape.StyledPanel)
        self.setStyleSheet(f"""
            QFrame#LinePanel {{
                background-color: {Palette.BG_PANEL};
                border: 1px solid {Palette.BORDER};
                border-radius: 12px;
            }}
        """)
        self.setObjectName("LinePanel")

        self.init_ui()

    def init_ui(self):
        main_layout = QVBoxLayout(self)
        main_layout.setContentsMargins(12, 12, 12, 12)
        main_layout.setSpacing(8)

        top_header_layout = QHBoxLayout()
        top_header_layout.setContentsMargins(0, 0, 0, 0)
        top_header_layout.setSpacing(6)

        self.lbl_header = QLabel(f"── {self.title} ──")
        self.lbl_header.setFont(QFont("Segoe UI", 12, QFont.Weight.Bold))
        self.lbl_header.setAlignment(Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignVCenter)
        self.lbl_header.setStyleSheet(f"color: {Palette.ACCENT_PINK}; margin-bottom: 0px;")
        top_header_layout.addWidget(self.lbl_header, stretch=1)

        self.btn_line_power = QPushButton()
        self.btn_line_power.setFont(QFont("Segoe UI", 9, QFont.Weight.Bold))
        self.btn_line_power.setFixedSize(96, 26)
        self.btn_line_power.clicked.connect(self.toggle_line_power)
        top_header_layout.addWidget(self.btn_line_power)

        main_layout.addLayout(top_header_layout)

        self.lbl_status = QLabel("Status: Menghubungkan...")
        self.lbl_status.setFont(QFont("Segoe UI", 9))
        self.lbl_status.setStyleSheet(f"color: {Palette.TEXT_SECONDARY}; margin-bottom: 2px;")
        main_layout.addWidget(self.lbl_status)

        # Audio Device Group
        self.grp_device = QGroupBox("🎙️ Audio Device")
        self.grp_device.setStyleSheet(groupbox_style())
        device_layout = QVBoxLayout(self.grp_device)
        device_layout.setSpacing(4)

        dev_top_row = QHBoxLayout()
        self.lbl_input = QLabel("Input (Mic / Line-In):")
        self.lbl_input.setStyleSheet(f"color: {Palette.TEXT_SECONDARY}; font-weight: bold;")
        dev_top_row.addWidget(self.lbl_input, stretch=1)

        self.btn_dev_refresh = QPushButton("🔄 Scan Perangkat")
        self.btn_dev_refresh.setFont(QFont("Segoe UI", 8))
        self.btn_dev_refresh.setFixedHeight(22)
        self.btn_dev_refresh.setStyleSheet(f"background: {Palette.BG_INPUT}; color: {Palette.TEXT_PRIMARY}; border: 1px solid {Palette.BORDER}; border-radius: 4px; padding: 2px 6px;")
        self.btn_dev_refresh.setToolTip("Scan ulang audio device jika baru colok mic atau speaker")
        self.btn_dev_refresh.clicked.connect(self.main_window.refresh_audio_devices)
        dev_top_row.addWidget(self.btn_dev_refresh)
        device_layout.addLayout(dev_top_row)

        self.cb_input = QComboBox()
        self.cb_input.setStyleSheet(combobox_style())
        self.populate_inputs()
        self.cb_input.currentIndexChanged.connect(self.on_device_changed)
        device_layout.addWidget(self.cb_input)

        self.lbl_output = QLabel("Output (Speaker / Cable Input):")
        self.lbl_output.setStyleSheet(f"color: {Palette.TEXT_SECONDARY}; font-weight: bold; margin-top: 4px;")
        device_layout.addWidget(self.lbl_output)

        self.cb_output = QComboBox()
        self.cb_output.setStyleSheet(combobox_style())
        self.populate_outputs()
        self.cb_output.currentIndexChanged.connect(self.on_device_changed)
        device_layout.addWidget(self.cb_output)

        main_layout.addWidget(self.grp_device)

        # VST Chain
        self.grp_vst = QGroupBox("🔌 VST Chain")
        self.grp_vst.setStyleSheet(groupbox_style())
        vst_layout_outer = QVBoxLayout(self.grp_vst)
        vst_layout_outer.setSpacing(6)

        self.scroll_area = QScrollArea()
        self.scroll_area.setWidgetResizable(True)
        self.scroll_area.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAlwaysOff)
        self.scroll_area.setStyleSheet(f"background: {Palette.BG_SCROLL}; border: 1px solid {Palette.BORDER}; border-radius: 8px;")
        self.scroll_area.setMinimumHeight(110)

        self.vst_container = QWidget()
        self.vst_layout = QVBoxLayout(self.vst_container)
        self.vst_layout.setContentsMargins(6, 6, 6, 6)
        self.vst_layout.setSpacing(6)
        self.vst_layout.setAlignment(Qt.AlignmentFlag.AlignTop)
        self.scroll_area.setWidget(self.vst_container)
        vst_layout_outer.addWidget(self.scroll_area, stretch=1)

        self.btn_add_vst = QPushButton("➕ Tambah VST3")
        self.btn_add_vst.setFont(QFont("Segoe UI", 10, QFont.Weight.Bold))
        self.btn_add_vst.setStyleSheet(self._add_vst_btn_style())
        self.btn_add_vst.clicked.connect(self.open_vst_selector)
        vst_layout_outer.addWidget(self.btn_add_vst)

        main_layout.addWidget(self.grp_vst, stretch=1)

        # Overlay Audio
        self.grp_overlay = QGroupBox("🗂️ Overlay Audio (Loop File)")
        self.grp_overlay.setStyleSheet(groupbox_style())
        overlay_layout = QVBoxLayout(self.grp_overlay)
        overlay_layout.setSpacing(4)

        self.overlay_in_rows_layout, self.lbl_overlay_in_hdr, self.btn_overlay_in_add, self.scroll_overlay_in = \
            self._build_overlay_section(overlay_layout, "🎙️ Input", "input")
        self.overlay_out_rows_layout, self.lbl_overlay_out_hdr, self.btn_overlay_out_add, self.scroll_overlay_out = \
            self._build_overlay_section(overlay_layout, "🔊 Output", "output")

        self.overlay_in_row_widgets = []
        self.overlay_out_row_widgets = []
        self.refresh_overlay_rows("input")
        self.refresh_overlay_rows("output")

        main_layout.addWidget(self.grp_overlay)

        # Master Volume
        self.grp_vol = QGroupBox("🔊 Master Volume")
        self.grp_vol.setStyleSheet(groupbox_style())
        vol_group_layout = QVBoxLayout(self.grp_vol)
        vol_group_layout.setSpacing(4)

        vol_layout = QHBoxLayout()
        vol_layout.setSpacing(6)

        self.btn_mute = QPushButton("🔊 MUTE")
        self.btn_mute.setFixedSize(70, 26)
        self.btn_mute.setFont(QFont("Segoe UI", 8, QFont.Weight.Bold))
        self.btn_mute.clicked.connect(self.toggle_mute)
        vol_layout.addWidget(self.btn_mute)
        self.update_mute_visual()

        self.btn_vol_down = QPushButton("◀")
        self.btn_vol_down.setFixedSize(26, 26)
        self.btn_vol_down.setStyleSheet(self._vol_btn_style())
        self.btn_vol_down.clicked.connect(lambda: self.adjust_volume(-1))
        vol_layout.addWidget(self.btn_vol_down)

        self.slider_vol = QSlider(Qt.Orientation.Horizontal)
        self.slider_vol.setRange(0, 150)
        self.slider_vol.setValue(100)
        self.slider_vol.setSingleStep(1)
        self.slider_vol.setPageStep(1)
        self.slider_vol.setStyleSheet(self._vol_slider_style())
        self.slider_vol.valueChanged.connect(self.on_volume_slider_changed)
        vol_layout.addWidget(self.slider_vol)

        self.btn_vol_up = QPushButton("▶")
        self.btn_vol_up.setFixedSize(26, 26)
        self.btn_vol_up.setStyleSheet(self._vol_btn_style())
        self.btn_vol_up.clicked.connect(lambda: self.adjust_volume(1))
        vol_layout.addWidget(self.btn_vol_up)

        self.lbl_vol_val = QLabel("100%")
        self.lbl_vol_val.setFixedWidth(42)
        self.lbl_vol_val.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        self.lbl_vol_val.setFont(QFont("Segoe UI", 9, QFont.Weight.Bold))
        self.lbl_vol_val.setStyleSheet(f"color: {Palette.ACCENT_PINK};")
        vol_layout.addWidget(self.lbl_vol_val)

        vol_group_layout.addLayout(vol_layout)

        self.vu_meter = VUMeterWidget()
        vol_group_layout.addWidget(self.vu_meter)

        main_layout.addWidget(self.grp_vol)
        self.update_line_power_visual()

    def toggle_line_power(self):
        new_state = not self.pipeline.is_enabled
        self.pipeline.set_enabled(new_state)
        self.update_line_power_visual()
        self.lbl_status.setText(self.pipeline.status_message)
        self.main_window.trigger_auto_save()

    def update_line_power_visual(self):
        if self.pipeline.is_enabled:
            self.btn_line_power.setText("🟢 LINE ON")
            self.btn_line_power.setStyleSheet(f"""
                QPushButton {{
                    background-color: {Palette.ACCENT_GREEN};
                    color: {Palette.TEXT_PRIMARY};
                    border: 1px solid {Palette.ACCENT_GREEN_DEEP};
                    border-radius: 6px;
                    font-weight: bold;
                }}
                QPushButton:hover {{ background-color: {Palette.ACCENT_GREEN_HOVER}; }}
            """)
            self.btn_line_power.setToolTip(f"Klik untuk mematikan {self.title} (stop audio & proses)")
        else:
            self.btn_line_power.setText("⚪ LINE OFF")
            self.btn_line_power.setStyleSheet(f"""
                QPushButton {{
                    background-color: {Palette.ACCENT_CORAL};
                    color: #FFFFFF;
                    border: 1px solid {Palette.ACCENT_CORAL_DEEP};
                    border-radius: 6px;
                    font-weight: bold;
                }}
                QPushButton:hover {{ background-color: {Palette.ACCENT_CORAL_HOVER}; }}
            """)
            self.btn_line_power.setToolTip(f"Klik untuk mengaktifkan kembali {self.title}")

    def _add_vst_btn_style(self):
        return f"""
            QPushButton {{
                background-color: {Palette.ACCENT_LAVENDER};
                color: {Palette.TEXT_PRIMARY};
                padding: 8px;
                border-radius: 8px;
            }}
            QPushButton:hover {{ background-color: {Palette.ACCENT_LAVENDER_HOVER}; }}
        """

    def _vol_btn_style(self):
        return f"background: {Palette.BG_INPUT}; color: {Palette.TEXT_PRIMARY}; border: 1px solid {Palette.BORDER}; border-radius: 6px;"

    def _vol_slider_style(self):
        return f"""
            QSlider::groove:horizontal {{ height: 6px; background: {Palette.BORDER}; border-radius: 3px; }}
            QSlider::sub-page:horizontal {{ background: {Palette.ACCENT_PINK}; border-radius: 3px; }}
            QSlider::handle:horizontal {{ background: #FFFFFF; border: 2px solid {Palette.ACCENT_PINK}; width: 12px; margin-top: -4px; margin-bottom: -4px; border-radius: 7px; }}
        """

    def refresh_theme(self):
        self.setStyleSheet(f"""
            QFrame#LinePanel {{
                background-color: {Palette.BG_PANEL};
                border: 1px solid {Palette.BORDER};
                border-radius: 12px;
            }}
        """)
        self.lbl_header.setStyleSheet(f"color: {Palette.ACCENT_PINK}; margin-bottom: 0px;")
        self.lbl_status.setStyleSheet(f"color: {Palette.TEXT_SECONDARY}; margin-bottom: 2px;")
        self.update_line_power_visual()

        for grp in (self.grp_device, self.grp_vst, self.grp_overlay, self.grp_vol):
            grp.setStyleSheet(groupbox_style())

        self.lbl_input.setStyleSheet(f"color: {Palette.TEXT_SECONDARY}; font-weight: bold;")
        self.lbl_output.setStyleSheet(f"color: {Palette.TEXT_SECONDARY}; font-weight: bold; margin-top: 4px;")
        self.btn_dev_refresh.setStyleSheet(f"background: {Palette.BG_INPUT}; color: {Palette.TEXT_PRIMARY}; border: 1px solid {Palette.BORDER}; border-radius: 4px; padding: 2px 6px;")
        self.cb_input.setStyleSheet(combobox_style())
        self.cb_output.setStyleSheet(combobox_style())

        self.scroll_area.setStyleSheet(f"background: {Palette.BG_SCROLL}; border: 1px solid {Palette.BORDER}; border-radius: 8px;")
        self.btn_add_vst.setStyleSheet(self._add_vst_btn_style())

        for card in getattr(self, "vst_card_widgets", []):
            card.refresh_theme()

        for lbl_hdr, btn_add, scroll, row_widgets in (
            (self.lbl_overlay_in_hdr, self.btn_overlay_in_add, self.scroll_overlay_in, self.overlay_in_row_widgets),
            (self.lbl_overlay_out_hdr, self.btn_overlay_out_add, self.scroll_overlay_out, self.overlay_out_row_widgets),
        ):
            lbl_hdr.setStyleSheet(f"color: {Palette.TEXT_PRIMARY}; font-weight: bold; font-size: 8.5pt;")
            scroll.setStyleSheet(f"background: {Palette.BG_SCROLL}; border: 1px solid {Palette.BORDER}; border-radius: 6px;")
            for row in row_widgets:
                row.refresh_theme()

        self.update_mute_visual()
        self.btn_vol_down.setStyleSheet(self._vol_btn_style())
        self.btn_vol_up.setStyleSheet(self._vol_btn_style())
        self.slider_vol.setStyleSheet(self._vol_slider_style())
        self.lbl_vol_val.setStyleSheet(f"color: {Palette.ACCENT_PINK};")
        self.vu_meter.update()

    def populate_inputs(self, select_device_name=None):
        self.cb_input.blockSignals(True)
        self.cb_input.clear()
        self.cb_input.addItem("🚫 (Tidak Ada / None)", None)
        target_idx = 0
        for i, (idx, dev_name, dev) in enumerate(self.wasapi_inputs, start=1):
            self.cb_input.addItem(f"{dev_name} [{dev['max_input_channels']} In]", (idx, dev_name))
            if select_device_name and dev_name == select_device_name:
                target_idx = i
        self.cb_input.setCurrentIndex(target_idx)
        self.cb_input.blockSignals(False)

    def populate_outputs(self, select_device_name=None):
        self.cb_output.blockSignals(True)
        self.cb_output.clear()
        self.cb_output.addItem("🚫 (Tidak Ada / None)", None)
        target_idx = 0
        for i, (idx, dev_name, dev) in enumerate(self.wasapi_outputs, start=1):
            self.cb_output.addItem(f"{dev_name} [{dev['max_output_channels']} Out]", (idx, dev_name))
            if select_device_name and dev_name == select_device_name:
                target_idx = i
        self.cb_output.setCurrentIndex(target_idx)
        self.cb_output.blockSignals(False)

    def on_device_changed(self):
        in_data = self.cb_input.currentData()
        out_data = self.cb_output.currentData()

        if not self.pipeline.is_enabled:
            self.pipeline.stop_stream()
            self.pipeline.input_device = in_data[0] if in_data else None
            self.pipeline.output_device = out_data[0] if out_data else None
            self.pipeline.status_message = "⚪ Line Dinonaktifkan (OFF)"
            self.lbl_status.setText(self.pipeline.status_message)
            self.main_window.trigger_auto_save()
            return

        if in_data and out_data:
            in_idx = in_data[0]
            out_idx = out_data[0]
            self.pipeline.update_devices(
                in_idx, out_idx, self.pipeline.sample_rate, self.pipeline.buffer_size
            )
        else:
            self.pipeline.stop_stream()
            self.pipeline.input_device = in_data[0] if in_data else None
            self.pipeline.output_device = out_data[0] if out_data else None
            self.pipeline.status_message = "⚪ Belum aktif - pilih Input & Output"

        self.lbl_status.setText(self.pipeline.status_message)
        self.main_window.trigger_auto_save()

    def toggle_mute(self):
        self.pipeline.is_muted = not self.pipeline.is_muted
        self.update_mute_visual()
        self.main_window.trigger_auto_save()

    def update_mute_visual(self):
        if self.pipeline.is_muted:
            self.btn_mute.setText("🔇 MUTED")
            self.btn_mute.setStyleSheet(f"""
                QPushButton {{
                    background-color: {Palette.ACCENT_CORAL};
                    color: #FFFFFF;
                    border: 1px solid {Palette.ACCENT_CORAL_DEEP};
                    border-radius: 6px;
                    font-weight: bold;
                }}
                QPushButton:hover {{ background-color: {Palette.ACCENT_CORAL_HOVER}; }}
            """)
        else:
            self.btn_mute.setText("🔊 MUTE")
            self.btn_mute.setStyleSheet(f"""
                QPushButton {{
                    background-color: {Palette.BG_INPUT};
                    color: {Palette.TEXT_PRIMARY};
                    border: 1px solid {Palette.BORDER};
                    border-radius: 6px;
                    font-weight: bold;
                }}
                QPushButton:hover {{ background-color: {Palette.ACCENT_PINK_SOFT}; }}
            """)

    def adjust_volume(self, delta):
        cur = self.slider_vol.value()
        new_val = max(0, min(150, cur + delta))
        self.slider_vol.setValue(new_val)

    def on_volume_slider_changed(self, value):
        self.lbl_vol_val.setText(f"{value}%")
        self.pipeline.master_gain = value / 100.0
        self.main_window.trigger_auto_save()

    def _build_overlay_section(self, parent_layout, header_text, kind):
        hdr_row = QHBoxLayout()
        hdr_row.setSpacing(4)
        lbl_hdr = QLabel(header_text)
        lbl_hdr.setStyleSheet(f"color: {Palette.TEXT_PRIMARY}; font-weight: bold; font-size: 8.5pt;")
        hdr_row.addWidget(lbl_hdr)
        hdr_row.addStretch()

        btn_add = QPushButton("➕")
        btn_add.setFixedSize(22, 20)
        btn_add.setToolTip("Tambah slot file (maks 10)")
        btn_add.clicked.connect(lambda checked=False, k=kind: self.on_overlay_add_slot(k))
        hdr_row.addWidget(btn_add)
        parent_layout.addLayout(hdr_row)

        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAlwaysOff)
        scroll.setFixedHeight(38 if OverlayBank.MAX_SLOTS <= 1 else 76)
        scroll.setStyleSheet(f"background: {Palette.BG_SCROLL}; border: 1px solid {Palette.BORDER}; border-radius: 6px;")

        container = QWidget()
        rows_layout = QVBoxLayout(container)
        rows_layout.setContentsMargins(3, 3, 3, 3)
        rows_layout.setSpacing(3)
        rows_layout.setAlignment(Qt.AlignmentFlag.AlignTop)
        scroll.setWidget(container)
        parent_layout.addWidget(scroll)

        return rows_layout, lbl_hdr, btn_add, scroll

    def refresh_overlay_rows(self, kind):
        bank = self.pipeline.overlay_input_bank if kind == "input" else self.pipeline.overlay_output_bank
        rows_layout = self.overlay_in_rows_layout if kind == "input" else self.overlay_out_rows_layout
        btn_add = self.btn_overlay_in_add if kind == "input" else self.btn_overlay_out_add

        while rows_layout.count() > 0:
            item = rows_layout.takeAt(0)
            w = item.widget()
            if w is not None:
                w.deleteLater()

        row_widgets = []
        can_remove = len(bank.players) > 1
        for idx, player in enumerate(bank.players):
            row = OverlaySlotRow(
                player=player, index=idx,
                on_toggle=lambda i, k=kind: self.on_overlay_slot_toggle(k, i),
                on_browse=lambda i, k=kind: self.on_overlay_slot_browse(k, i),
                on_gain_changed=lambda i, v, k=kind: self.on_overlay_slot_gain(k, i, v),
                on_remove=lambda i, k=kind: self.on_overlay_slot_remove(k, i),
                can_remove=can_remove,
            )
            rows_layout.addWidget(row)
            row_widgets.append(row)

        if kind == "input":
            self.overlay_in_row_widgets = row_widgets
        else:
            self.overlay_out_row_widgets = row_widgets

        btn_add.setVisible(OverlayBank.MAX_SLOTS > 1)
        btn_add.setEnabled(len(bank.players) < OverlayBank.MAX_SLOTS)

    def on_overlay_add_slot(self, kind):
        bank = self.pipeline.overlay_input_bank if kind == "input" else self.pipeline.overlay_output_bank
        if bank.add_slot():
            self.refresh_overlay_rows(kind)
            self.main_window.trigger_auto_save()

    def on_overlay_slot_remove(self, kind, index):
        bank = self.pipeline.overlay_input_bank if kind == "input" else self.pipeline.overlay_output_bank
        if bank.remove_slot(index):
            self.refresh_overlay_rows(kind)
            self.main_window.trigger_auto_save()

    def on_overlay_slot_toggle(self, kind, index):
        bank = self.pipeline.overlay_input_bank if kind == "input" else self.pipeline.overlay_output_bank
        row_widgets = self.overlay_in_row_widgets if kind == "input" else self.overlay_out_row_widgets
        if 0 <= index < len(bank.players) and index < len(row_widgets):
            checked = row_widgets[index].chk.isChecked()
            bank.players[index].set_enabled(checked)
            self.main_window.trigger_auto_save()

    def on_overlay_slot_browse(self, kind, index):
        bank = self.pipeline.overlay_input_bank if kind == "input" else self.pipeline.overlay_output_bank
        row_widgets = self.overlay_in_row_widgets if kind == "input" else self.overlay_out_row_widgets
        if not (0 <= index < len(bank.players) and index < len(row_widgets)):
            return
        player = bank.players[index]

        path, _ = QFileDialog.getOpenFileName(
            self, "Pilih File Audio Overlay", "",
            "Audio Files (*.mp3 *.wav *.ogg *.flac *.aif *.aiff)"
        )
        if not path:
            return

        ok, info = player.load_file(path, self.pipeline.sample_rate)
        if ok:
            row_widgets[index].lbl_file.setText(f"{os.path.basename(path)} ({info:.1f}s)")
        else:
            row_widgets[index].lbl_file.setText("Gagal load file!")
            QMessageBox.warning(self, "Overlay Audio", f"Gagal memuat file:\n{info}")
        self.main_window.trigger_auto_save()

    def on_overlay_slot_gain(self, kind, index, value):
        bank = self.pipeline.overlay_input_bank if kind == "input" else self.pipeline.overlay_output_bank
        if 0 <= index < len(bank.players):
            bank.players[index].set_gain(value / 100.0)
            self.main_window.trigger_auto_save()

    def open_vst_selector(self):
        dialog = VSTSelectorDialog(self, self.main_window.scanned_vst_cache)
        if dialog.exec() == QDialog.DialogCode.Accepted and dialog.selected_path:
            try:
                self.pipeline.add_vst(dialog.selected_path)
                self.refresh_vst_cards()
                self.main_window.trigger_auto_save()
            except Exception as e:
                QMessageBox.critical(self, "Gagal Memuat VST", f"VST ini tidak dapat dimuat:\n{str(e)}")

    def refresh_vst_cards(self):
        while self.vst_layout.count() > 0:
            item = self.vst_layout.takeAt(0)
            widget = item.widget()
            if widget is not None:
                widget.deleteLater()

        self.vst_card_widgets = []
        for idx, slot in enumerate(self.pipeline.vst_slots):
            card = VSTCardWidget(
                slot=slot,
                index=idx,
                on_toggle_bypass=self.toggle_bypass,
                on_edit=self.edit_vst_direct,
                on_move_up=self.move_up,
                on_move_down=self.move_down,
                on_delete=self.delete_vst
            )
            self.vst_layout.addWidget(card)
            self.vst_card_widgets.append(card)

    def refresh_all_error_indicators(self):
        for card in getattr(self, "vst_card_widgets", []):
            card.refresh_error_indicator()

    def toggle_bypass(self, index, card_widget):
        if 0 <= index < len(self.pipeline.vst_slots):
            slot = self.pipeline.vst_slots[index]
            with self.pipeline.lock:
                slot.bypassed = not slot.bypassed
                if not slot.bypassed and slot.plugin is not None:
                    try:
                        slot.plugin.reset()
                    except Exception:
                        pass
                    slot.latency_buffer = None
            card_widget.update_bypass_visual()
            self.main_window.trigger_auto_save()

    def edit_vst_direct(self, slot):
        if self._is_editing_vst:
            return
        self._is_editing_vst = True

        try:
            if hasattr(slot.plugin, "show_editor"):
                main_hwnd = int(self.main_window.winId())

                if sys.platform == "win32":
                    known_before = _enum_process_windows(main_hwnd)

                    def _delayed_titlebar_fix():
                        time.sleep(0.06)
                        fix_vst_window_titlebar(slot.name, main_hwnd, known_before)
                        time.sleep(0.19)
                        fix_vst_window_titlebar(slot.name, main_hwnd, known_before)

                    threading.Thread(target=_delayed_titlebar_fix, daemon=True).start()

                self.lbl_status.setText(f"🎛️ Editor '{slot.name}' terbuka - tutup jendela plugin untuk lanjut")
                QApplication.processEvents()

                slot.plugin.show_editor()

                slot.cached_params = capture_vst_parameters(slot.plugin)
                self.lbl_status.setText(self.pipeline.status_message)
                self.main_window.trigger_auto_save()
            else:
                QMessageBox.information(
                    self, "Info", f"Plugin {slot.name} tidak memiliki GUI kustom terpisah."
                )
        except Exception as e:
            QMessageBox.warning(self, "Error GUI VST", f"Gagal membuka jendela GUI plugin: {e}")
        finally:
            self._is_editing_vst = False

    def move_up(self, index):
        if index > 0:
            self.pipeline.move_vst(index, -1)
            self.refresh_vst_cards()
            self.main_window.trigger_auto_save()

    def move_down(self, index):
        if index < len(self.pipeline.vst_slots) - 1:
            self.pipeline.move_vst(index, 1)
            self.refresh_vst_cards()
            self.main_window.trigger_auto_save()

    def delete_vst(self, index):
        self.pipeline.remove_vst(index)
        self.refresh_vst_cards()
        self.main_window.trigger_auto_save()


# ==============================================================================
# 8. MAIN WINDOW
# ==============================================================================

class RinkaSoundBridgeWindow(QMainWindow):
    def __init__(self):
        super().__init__()
        self.setWindowTitle("Rinka SoundBridge v1.21 2026.09")
        self.resize(1020, 940)
        self.setMinimumSize(600, 500)
        self.is_loading_config = False
        self._is_refreshing_devices = False
        self._is_shutting_down = False

        self.setStyleSheet(f"""
            QMainWindow {{ background-color: {Palette.BG_APP}; }}
            QLabel {{ color: {Palette.TEXT_PRIMARY}; }}
        """)

        self.wasapi_inputs, self.wasapi_outputs = get_wasapi_devices()

        self.pipeline_a = AudioPipeline("Line A")
        self.pipeline_b = AudioPipeline("Line B")

        self.scanned_vst_cache = VSTScanner.scan_plugins()

        self.init_ui()
        self.init_tray()
        self.load_config()

        self.vu_timer = QTimer(self)
        self.vu_timer.setInterval(33)
        self.vu_timer.timeout.connect(self.update_vu_meters)
        self.vu_timer.start()

        self.latency_timer = QTimer(self)
        self.latency_timer.setInterval(3000)
        self.latency_timer.timeout.connect(self.update_latency_meters)
        self.latency_timer.start()

        self.vst_error_timer = QTimer(self)
        self.vst_error_timer.setInterval(1000)
        self.vst_error_timer.timeout.connect(self.update_vst_error_indicators)
        self.vst_error_timer.start()

        if PSUTIL_AVAILABLE:
            self._perf_process = psutil.Process(os.getpid())
            self._perf_process.cpu_percent(interval=None)
            self.perf_timer = QTimer(self)
            self.perf_timer.setInterval(1000)
            self.perf_timer.timeout.connect(self.update_performance_indicator)
            self.perf_timer.start()

    def init_ui(self):
        central_widget = QWidget()
        self.setCentralWidget(central_widget)
        root_layout = QVBoxLayout(central_widget)
        root_layout.setContentsMargins(16, 16, 16, 16)
        root_layout.setSpacing(12)

        header_frame = QFrame()
        self.header_frame = header_frame
        header_frame.setStyleSheet(f"""
            QFrame {{
                background-color: {Palette.BG_PANEL_ALT};
                border: 1px solid {Palette.BORDER};
                border-radius: 12px;
                padding: 6px;
            }}
        """)
        header_layout = QVBoxLayout(header_frame)
        header_layout.setSpacing(8)

        top_ctrl_layout = QHBoxLayout()
        self.lbl_perf = QLabel("🖥️ CPU: --%  |  RAM: -- MB")
        self.lbl_perf.setFont(QFont("Segoe UI", 10, QFont.Weight.Bold))
        self.lbl_perf.setStyleSheet(f"color: {Palette.ACCENT_GREEN_DEEP};")
        top_ctrl_layout.addWidget(self.lbl_perf)
        if not PSUTIL_AVAILABLE:
            self.lbl_perf.setText("🖥️ Performa: N/A (pip install psutil)")
            self.lbl_perf.setStyleSheet(f"color: {Palette.TEXT_MUTED};")

        top_ctrl_layout.addStretch()

        self.lbl_buf = QLabel("Buffer Size:")
        self.lbl_buf.setStyleSheet(f"color: {Palette.TEXT_SECONDARY};")
        top_ctrl_layout.addWidget(self.lbl_buf)

        self.cb_buffer = QComboBox()
        self.cb_buffer.setStyleSheet(combobox_style())
        for buf in [64, 128, 256, 512, 1024]:
            ms = round((buf / 48000.0) * 1000, 1)
            self.cb_buffer.addItem(f"{buf} Samples ({ms} ms)", buf)
        self.cb_buffer.setCurrentIndex(2)
        self.cb_buffer.currentIndexChanged.connect(self.on_global_audio_settings_changed)
        top_ctrl_layout.addWidget(self.cb_buffer)

        self.lbl_sr = QLabel("Sample Rate:")
        self.lbl_sr.setStyleSheet(f"color: {Palette.TEXT_SECONDARY}; margin-left: 8px;")
        top_ctrl_layout.addWidget(self.lbl_sr)

        self.cb_sr = QComboBox()
        self.cb_sr.setStyleSheet(combobox_style())
        self.cb_sr.addItem("44100 Hz", 44100)
        self.cb_sr.addItem("48000 Hz", 48000)
        self.cb_sr.setCurrentIndex(1)
        self.cb_sr.currentIndexChanged.connect(self.on_global_audio_settings_changed)
        top_ctrl_layout.addWidget(self.cb_sr)

        header_layout.addLayout(top_ctrl_layout)

        tray_row_layout = QHBoxLayout()
        tray_row_layout.setSpacing(8)

        self.cb_theme = QComboBox()
        self.cb_theme.setStyleSheet(combobox_style())
        self.cb_theme.setFixedWidth(180)
        self.cb_theme.setToolTip("Ganti mood tampilan aplikasi")
        for theme_name in THEMES.keys():
            self.cb_theme.addItem(f"🎨 {theme_name}", theme_name)
        idx = self.cb_theme.findData(CURRENT_THEME_NAME)
        if idx >= 0:
            self.cb_theme.setCurrentIndex(idx)
        self.cb_theme.currentIndexChanged.connect(self.on_theme_changed)
        tray_row_layout.addWidget(self.cb_theme)

        tray_box = QFrame()
        self.tray_box = tray_box
        tray_box.setStyleSheet(f"""
            QFrame {{
                background-color: {Palette.BG_PANEL};
                border: 1px dashed {Palette.BORDER_STRONG};
                border-radius: 8px;
                padding: 4px;
            }}
        """)
        tray_box_layout = QHBoxLayout(tray_box)
        tray_box_layout.setContentsMargins(6, 4, 6, 4)
        tray_box_layout.setSpacing(6)

        self.btn_hide_tray = QPushButton("📥 SYSTEM TRAY (RUN IN BACKGROUND)")
        self.btn_hide_tray.setFont(QFont("Segoe UI", 9, QFont.Weight.Bold))
        self.btn_hide_tray.setStyleSheet(self._hide_tray_btn_style())
        self.btn_hide_tray.clicked.connect(self.minimize_to_tray)
        tray_box_layout.addWidget(self.btn_hide_tray, stretch=1)

        # Tombol Khusus Refresh Audio Device (Tanpa Restart)
        self.btn_refresh_devices = QPushButton("🎧 REFRESH AUDIO DEVICE")
        self.btn_refresh_devices.setFont(QFont("Segoe UI", 9, QFont.Weight.Bold))
        self.btn_refresh_devices.setToolTip("Scan ulang microphone & speaker (WASAPI) yang baru dicolokkan tanpa me-restart aplikasi")
        self.btn_refresh_devices.setStyleSheet(self._refresh_devices_btn_style())
        self.btn_refresh_devices.clicked.connect(self.refresh_audio_devices)
        tray_box_layout.addWidget(self.btn_refresh_devices)

        # Tombol Restart Total Aplikasi
        self.btn_reload = QPushButton("⚡ RESTART APP")
        self.btn_reload.setFont(QFont("Segoe UI", 9, QFont.Weight.Bold))
        self.btn_reload.setFixedWidth(115)
        self.btn_reload.setToolTip("Restart total aplikasi dan audio engine (dengan proteksi anti-crash)")
        self.btn_reload.setStyleSheet(self._reload_btn_style())
        self.btn_reload.clicked.connect(self.reload_application)
        tray_box_layout.addWidget(self.btn_reload)

        tray_row_layout.addWidget(tray_box, stretch=1)
        header_layout.addLayout(tray_row_layout)
        root_layout.addWidget(header_frame)

        channels_layout = QHBoxLayout()
        channels_layout.setSpacing(12)

        self.panel_line_a = LinePanelWidget("Line A", self.pipeline_a, self.wasapi_inputs, self.wasapi_outputs, self)
        self.panel_line_b = LinePanelWidget("Line B", self.pipeline_b, self.wasapi_inputs, self.wasapi_outputs, self)

        channels_layout.addWidget(self.panel_line_a, stretch=1)
        channels_layout.addWidget(self.panel_line_b, stretch=1)

        root_layout.addLayout(channels_layout, stretch=1)

        footer = QFrame()
        self.footer = footer
        footer.setStyleSheet(f"QFrame {{ border-top: 1px solid {Palette.BORDER}; }}")
        footer_layout = QHBoxLayout(footer)
        footer_layout.setContentsMargins(6, 6, 6, 6)
        footer_layout.setSpacing(8)
        footer_layout.addStretch()

        self._footer_dim_labels = []

        # Label teks diperbesar ke ukuran 10 Bold
        lbl_tag = QLabel("🎙️ Total Delay Voice Changer (In A ➔ Out B):")
        lbl_tag.setFont(QFont("Segoe UI", 10, QFont.Weight.Bold))
        lbl_tag.setStyleSheet(f"color: {Palette.TEXT_SECONDARY};")
        self._footer_dim_labels.append(lbl_tag)
        footer_layout.addWidget(lbl_tag)

        # Angka milidetik (ms) diperbesar ke ukuran 13 Bold
        self.lbl_lat_in_out = QLabel("-- ms")
        self.lbl_lat_in_out.setFont(QFont("Segoe UI", 13, QFont.Weight.Bold))
        self.lbl_lat_in_out.setStyleSheet(f"color: {Palette.ACCENT_PINK};")
        footer_layout.addWidget(self.lbl_lat_in_out)

        self._lat_hold_state = {
            "in_out": {"peak": None, "last_updated": 0.0},
        }

        footer_layout.addStretch()
        root_layout.addWidget(footer)

    def on_global_audio_settings_changed(self):
        buf = self.cb_buffer.currentData()
        sr = self.cb_sr.currentData()
        self.pipeline_a.buffer_size = buf
        self.pipeline_a.sample_rate = sr
        self.pipeline_b.buffer_size = buf
        self.pipeline_b.sample_rate = sr

        self.panel_line_a.on_device_changed()
        self.panel_line_b.on_device_changed()
        self.trigger_auto_save()

    def _hide_tray_btn_style(self):
        return f"""
            QPushButton {{
                background-color: {Palette.ACCENT_BLUE};
                color: {Palette.TEXT_PRIMARY};
                padding: 7px 10px;
                border-radius: 8px;
            }}
            QPushButton:hover {{ background-color: {Palette.ACCENT_BLUE_HOVER}; }}
        """

    def _refresh_devices_btn_style(self):
        return f"""
            QPushButton {{
                background-color: {Palette.ACCENT_GREEN};
                color: {Palette.TEXT_PRIMARY};
                padding: 7px 12px;
                border-radius: 8px;
                font-weight: bold;
            }}
            QPushButton:hover {{ background-color: {Palette.ACCENT_GREEN_HOVER}; }}
            QPushButton:disabled {{ background-color: {Palette.BORDER}; color: {Palette.TEXT_MUTED}; }}
        """

    def _reload_btn_style(self):
        return f"""
            QPushButton {{
                background-color: {Palette.ACCENT_YELLOW};
                color: {Palette.TEXT_PRIMARY};
                padding: 7px 10px;
                border-radius: 8px;
                font-weight: bold;
            }}
            QPushButton:hover {{ background-color: {Palette.ACCENT_PINK_SOFT}; }}
            QPushButton:disabled {{ background-color: {Palette.BORDER}; color: {Palette.TEXT_MUTED}; }}
        """

    def on_theme_changed(self):
        theme_name = self.cb_theme.currentData()
        if theme_name:
            apply_theme(theme_name, refresh_window=self)
            self.trigger_auto_save()

    def refresh_theme(self):
        self.setStyleSheet(f"""
            QMainWindow {{ background-color: {Palette.BG_APP}; }}
            QLabel {{ color: {Palette.TEXT_PRIMARY}; }}
        """)
        self.header_frame.setStyleSheet(f"""
            QFrame {{
                background-color: {Palette.BG_PANEL_ALT};
                border: 1px solid {Palette.BORDER};
                border-radius: 12px;
                padding: 6px;
            }}
        """)
        if PSUTIL_AVAILABLE:
            self.lbl_perf.setStyleSheet(f"color: {Palette.ACCENT_GREEN_DEEP};")
        else:
            self.lbl_perf.setStyleSheet(f"color: {Palette.TEXT_MUTED};")
        self.lbl_buf.setStyleSheet(f"color: {Palette.TEXT_SECONDARY};")
        self.lbl_sr.setStyleSheet(f"color: {Palette.TEXT_SECONDARY}; margin-left: 8px;")
        self.cb_buffer.setStyleSheet(combobox_style())
        self.cb_sr.setStyleSheet(combobox_style())
        self.cb_theme.setStyleSheet(combobox_style())

        self.tray_box.setStyleSheet(f"""
            QFrame {{
                background-color: {Palette.BG_PANEL};
                border: 1px dashed {Palette.BORDER_STRONG};
                border-radius: 8px;
                padding: 4px;
            }}
        """)
        self.btn_hide_tray.setStyleSheet(self._hide_tray_btn_style())
        self.btn_refresh_devices.setStyleSheet(self._refresh_devices_btn_style())
        self.btn_reload.setStyleSheet(self._reload_btn_style())

        self.panel_line_a.refresh_theme()
        self.panel_line_b.refresh_theme()

        if hasattr(self, "lbl_lat_in_out"):
            self.lbl_lat_in_out.setStyleSheet(f"color: {Palette.ACCENT_PINK};")
        for lbl in getattr(self, "_footer_dim_labels", []):
            lbl.setStyleSheet(f"color: {Palette.TEXT_SECONDARY};")
            self.footer.setStyleSheet(f"QFrame {{ border-top: 1px solid {Palette.BORDER}; }}")

    def update_vu_meters(self):
        if self.isVisible():
            self.panel_line_a.vu_meter.set_level(self.pipeline_a.peak_level)
            self.panel_line_b.vu_meter.set_level(self.pipeline_b.peak_level)

    def update_vst_error_indicators(self):
        if self.isVisible():
            self.panel_line_a.refresh_all_error_indicators()
            self.panel_line_b.refresh_all_error_indicators()

    def update_performance_indicator(self):
        if not PSUTIL_AVAILABLE:
            return
        try:
            # 1. Selalu hitung tiap detik agar delta waktu tetap sinkron & tidak loncat saat dari tray
            raw_cpu = self._perf_process.cpu_percent(interval=None)
            
            # 2. Normalisasi dengan total core/thread agar angkanya sama persis dengan Windows Task Manager
            total_cores = psutil.cpu_count(logical=True) or 1
            cpu_pct = raw_cpu / total_cores
            
            ram_mb = self._perf_process.memory_info().rss / (1024 * 1024)

            # 3. Update teks label hanya saat jendela sedang tampil di layar
            if self.isVisible():
                self.lbl_perf.setText(f"🖥️ CPU: {cpu_pct:.1f}%  |  RAM: {ram_mb:.0f} MB")
        except Exception:
            pass

    def _update_latency_metric(self, key, lbl_widget, new_val):
        now = time.monotonic()
        state = self._lat_hold_state[key]

        if state["peak"] is not None and (now - state["last_updated"] >= 5.0):
            state["peak"] = None

        if new_val is not None:
            val = abs(new_val)
            if state["peak"] is None or val > state["peak"]:
                state["peak"] = val
                state["last_updated"] = now

        if state["peak"] is None:
            lbl_widget.setText("-- ms")
        else:
            lbl_widget.setText(f"{state['peak']:.1f} ms")

    def update_latency_meters(self):
        if not self.isVisible() or not self.pipeline_a.is_enabled or not self.pipeline_b.is_enabled:
            self.lbl_lat_in_out.setText("-- ms")
            return

        total_delay = estimate_latency_ms(
            self.pipeline_a.probe_in, 
            self.pipeline_b.probe_out, 
            max_lag_ms=1500.0
        )
        self._update_latency_metric("in_out", self.lbl_lat_in_out, total_delay)

    def trigger_auto_save(self):
        if self.is_loading_config or self._is_shutting_down:
            return
        if not hasattr(self, "_save_debounce_timer"):
            self._save_debounce_timer = QTimer(self)
            self._save_debounce_timer.setSingleShot(True)
            self._save_debounce_timer.timeout.connect(self.save_config_now)
        self._save_debounce_timer.start(400)

    def save_config_now(self):
        if self.is_loading_config:
            return
        if hasattr(self, "_save_debounce_timer"):
            self._save_debounce_timer.stop()

        in_a = self.panel_line_a.cb_input.currentData()
        out_a = self.panel_line_a.cb_output.currentData()
        in_b = self.panel_line_b.cb_input.currentData()
        out_b = self.panel_line_b.cb_output.currentData()

        def _serialize_vst_slots(pipeline):
            result = []
            with pipeline.lock:
                for slot in pipeline.vst_slots:
                    result.append({
                        "path": slot.file_path,
                        "bypassed": slot.bypassed,
                        "params": dict(slot.cached_params) if slot.cached_params else {}
                    })
            return result

        def _serialize_overlay_bank(bank):
            return [
                {"enabled": p.enabled, "file_path": p.file_path, "gain": p.gain}
                for p in bank.players
            ]

        geo = self.geometry()
        geo_data = bytes(self.saveGeometry().toHex()).decode("ascii")
        config = {
            "global": {
                "buffer_size": self.cb_buffer.currentData(),
                "sample_rate": self.cb_sr.currentData(),
                "window_geometry": geo_data,
                "window_x": geo.x(),
                "window_y": geo.y(),
                "window_w": geo.width(),
                "window_h": geo.height(),
                "theme": CURRENT_THEME_NAME,
            },
            "line_a": {
                "is_enabled": self.pipeline_a.is_enabled,
                "input_device_name": in_a[1] if in_a else None,
                "output_device_name": out_a[1] if out_a else None,
                "master_volume": self.panel_line_a.slider_vol.value(),
                "is_muted": self.pipeline_a.is_muted,
                "overlay_input": _serialize_overlay_bank(self.pipeline_a.overlay_input_bank),
                "overlay_output": _serialize_overlay_bank(self.pipeline_a.overlay_output_bank),
                "vst_slots": _serialize_vst_slots(self.pipeline_a)
            },
            "line_b": {
                "is_enabled": self.pipeline_b.is_enabled,
                "input_device_name": in_b[1] if in_b else None,
                "output_device_name": out_b[1] if out_b else None,
                "master_volume": self.panel_line_b.slider_vol.value(),
                "is_muted": self.pipeline_b.is_muted,
                "overlay_input": _serialize_overlay_bank(self.pipeline_b.overlay_input_bank),
                "overlay_output": _serialize_overlay_bank(self.pipeline_b.overlay_output_bank),
                "vst_slots": _serialize_vst_slots(self.pipeline_b)
            }
        }

        try:
            with open(CONFIG_FILE, "w", encoding="utf-8") as f:
                json.dump(config, f, indent=4)
        except Exception as e:
            rinka_logger.error(f"Error Auto-Save: {e}")

    def load_config(self):
        if not os.path.exists(CONFIG_FILE):
            self.panel_line_a.on_device_changed()
            self.panel_line_b.on_device_changed()
            return

        self.is_loading_config = True
        try:
            with open(CONFIG_FILE, "r", encoding="utf-8") as f:
                config = json.load(f)

            g_cfg = config.get("global", {})
            buf = g_cfg.get("buffer_size", 256)
            sr = g_cfg.get("sample_rate", 48000)

            geo_hex = g_cfg.get("window_geometry")
            if geo_hex:
                try:
                    self.restoreGeometry(QByteArray.fromHex(geo_hex.encode("ascii")))
                except Exception as geo_err:
                    rinka_logger.warning(f"Gagal restore window geometry: {geo_err}")
            else:
                w_geo = g_cfg.get("window_w")
                h_geo = g_cfg.get("window_h")
                x_geo = g_cfg.get("window_x")
                y_geo = g_cfg.get("window_y")
                if w_geo and h_geo:
                    try:
                        self.resize(int(w_geo), int(h_geo))
                        if x_geo is not None and y_geo is not None:
                            self.move(int(x_geo), int(y_geo))
                    except Exception:
                        pass

            theme_name = g_cfg.get("theme", DEFAULT_THEME_NAME)
            if theme_name in THEMES:
                apply_theme(theme_name, refresh_window=self)
                idx = self.cb_theme.findData(theme_name)
                if idx >= 0:
                    self.cb_theme.setCurrentIndex(idx)

            for i in range(self.cb_buffer.count()):
                if self.cb_buffer.itemData(i) == buf:
                    self.cb_buffer.setCurrentIndex(i)
                    break

            for i in range(self.cb_sr.count()):
                if self.cb_sr.itemData(i) == sr:
                    self.cb_sr.setCurrentIndex(i)
                    break

            self.pipeline_a.buffer_size = buf
            self.pipeline_a.sample_rate = sr
            self.pipeline_b.buffer_size = buf
            self.pipeline_b.sample_rate = sr

            self._restore_line(self.panel_line_a, config.get("line_a", {}))
            self._restore_line(self.panel_line_b, config.get("line_b", {}))

        except Exception as e:
            rinka_logger.error(f"Error load config: {e}")
        finally:
            self.is_loading_config = False
            self.panel_line_a.on_device_changed()
            self.panel_line_b.on_device_changed()

    def _restore_line(self, panel, data):
        if not data:
            return

        is_enabled = data.get("is_enabled", True)
        panel.pipeline.is_enabled = is_enabled
        panel.update_line_power_visual()

        vol = data.get("master_volume", 100)
        panel.slider_vol.setValue(vol)

        is_muted = data.get("is_muted", False)
        panel.pipeline.is_muted = is_muted
        panel.update_mute_visual()

        in_name = data.get("input_device_name")
        out_name = data.get("output_device_name")

        panel.populate_inputs(select_device_name=in_name)
        panel.populate_outputs(select_device_name=out_name)

        for kind_key, kind_ui in (("overlay_input", "input"), ("overlay_output", "output")):
            bank = panel.pipeline.overlay_input_bank if kind_ui == "input" else panel.pipeline.overlay_output_bank
            data_list = data.get(kind_key, [])
            if not data_list:
                data_list = [{"enabled": False, "file_path": None, "gain": 0.5}]

            while len(bank.players) < min(len(data_list), OverlayBank.MAX_SLOTS):
                if not bank.add_slot():
                    break
            while len(bank.players) > len(data_list) and len(bank.players) > 1:
                bank.players.pop()

            for player, item in zip(bank.players, data_list):
                gain = item.get("gain", 0.5)
                player.set_gain(gain)

                file_path = item.get("file_path")
                if file_path and os.path.exists(file_path):
                    player.load_file(file_path, panel.pipeline.sample_rate)

                player.set_enabled(item.get("enabled", False))

            panel.refresh_overlay_rows(kind_ui)

        slots = data.get("vst_slots", [])
        for item in slots:
            path = item.get("path")
            bypassed = item.get("bypassed", False)
            params = item.get("params")
            if path and os.path.exists(path):
                try:
                    slot = panel.pipeline.add_vst(path)
                    slot.bypassed = bypassed
                    if params and slot.plugin is not None:
                        with panel.pipeline.lock:
                            restore_vst_parameters(slot.plugin, params)
                        slot.cached_params = dict(params)
                except Exception as err:
                    rinka_logger.error(f"Gagal restore VST {path}: {err}")

        panel.refresh_vst_cards()

    def init_tray(self):
        self.tray_icon = QSystemTrayIcon(self)
        self.tray_icon.setIcon(self.style().standardIcon(self.style().StandardPixmap.SP_MediaVolume))

        tray_menu = QMenu()
        show_action = QAction("Buka Rinka SoundBridge", self)
        show_action.triggered.connect(self.restore_from_tray)
        tray_menu.addAction(show_action)

        tray_menu.addSeparator()

        quit_action = QAction("Keluar Total", self)
        quit_action.triggered.connect(self.close_application)
        tray_menu.addAction(quit_action)

        self.tray_icon.setContextMenu(tray_menu)
        self.tray_icon.activated.connect(self.on_tray_activated)

    def minimize_to_tray(self):
        self.hide()
        self.tray_icon.show()
        self.tray_icon.showMessage(
            "Rinka SoundBridge",
            "Audio engine (WASAPI) tetap aktif di latar belakang.",
            QSystemTrayIcon.MessageIcon.Information,
            2000
        )

    def restore_from_tray(self):
        self.show()
        self.raise_()
        self.activateWindow()

    def on_tray_activated(self, reason):
        if reason == QSystemTrayIcon.ActivationReason.DoubleClick:
            self.restore_from_tray()

    # ==========================================================================
    # SISTEM RELOAD AUDIO DEVICE (TANPA RESTART APLIKASI)
    # ==========================================================================
    def refresh_audio_devices(self):
        if self._is_refreshing_devices or self._is_shutting_down:
            return
        self._is_refreshing_devices = True
        self.btn_refresh_devices.setEnabled(False)
        self.btn_refresh_devices.setText("⏳ Scanning...")
        QApplication.processEvents()

        try:
            # 1. Catat nama hardware audio yang sedang aktif dipilih saat ini
            in_a_name = self.panel_line_a.cb_input.currentData()[1] if self.panel_line_a.cb_input.currentData() else None
            out_a_name = self.panel_line_a.cb_output.currentData()[1] if self.panel_line_a.cb_output.currentData() else None
            in_b_name = self.panel_line_b.cb_input.currentData()[1] if self.panel_line_b.cb_input.currentData() else None
            out_b_name = self.panel_line_b.cb_output.currentData()[1] if self.panel_line_b.cb_output.currentData() else None

            # 2. Hentikan aliran stream aktif sesaat agar PortAudio bisa di-reinisialisasi
            self.pipeline_a.stop_stream()
            self.pipeline_b.stop_stream()

            # 3. Re-inisialisasi driver sounddevice untuk mendeteksi penambahan mic/speaker baru di Windows
            try:
                time.sleep(0.1)
                sd._terminate()
                time.sleep(0.15)
                sd._initialize()
            except Exception as pe:
                rinka_logger.error(f"Gagal re-init sounddevice WASAPI: {pe}")

            # 4. Ambil daftar perangkat WASAPI yang baru dipindai
            self.wasapi_inputs, self.wasapi_outputs = get_wasapi_devices()

            # 5. Pasang kembali ke Line A & B dengan mempertahankan pilihan nama device lama
            self.panel_line_a.wasapi_inputs = self.wasapi_inputs
            self.panel_line_a.wasapi_outputs = self.wasapi_outputs
            self.panel_line_a.populate_inputs(select_device_name=in_a_name)
            self.panel_line_a.populate_outputs(select_device_name=out_a_name)

            self.panel_line_b.wasapi_inputs = self.wasapi_inputs
            self.panel_line_b.wasapi_outputs = self.wasapi_outputs
            self.panel_line_b.populate_inputs(select_device_name=in_b_name)
            self.panel_line_b.populate_outputs(select_device_name=out_b_name)

            # 6. Jalankan kembali aliran audio WASAPI
            self.panel_line_a.on_device_changed()
            self.panel_line_b.on_device_changed()

            n_in = len(self.wasapi_inputs)
            n_out = len(self.wasapi_outputs)
            rinka_logger.info(f"Audio device berhasil di-refresh: {n_in} Input & {n_out} Output terdeteksi.")
        except Exception as e:
            rinka_logger.error(f"Gagal refresh audio device: {e}")
            QMessageBox.warning(self, "Peringatan", f"Gagal memperbarui daftar perangkat audio:\n{e}")
        finally:
            self.btn_refresh_devices.setText("🎧 REFRESH AUDIO DEVICE")
            self.btn_refresh_devices.setEnabled(True)
            self._is_refreshing_devices = False

    # ==========================================================================
    # CLEAN SHUTDOWN & SAFE DELAYED RESTART (ANTI-CRASH)
    # ==========================================================================
    def clean_shutdown(self):
        if self._is_shutting_down:
            return
        self._is_shutting_down = True

        # 1. Hentikan seluruh timer UI agar tidak membaca memory yang sedang dilepas
        if hasattr(self, "vu_timer"):
            self.vu_timer.stop()
        if hasattr(self, "latency_timer"):
            self.latency_timer.stop()
        if hasattr(self, "vst_error_timer"):
            self.vst_error_timer.stop()
        if hasattr(self, "perf_timer"):
            self.perf_timer.stop()
        if hasattr(self, "_save_debounce_timer"):
            self._save_debounce_timer.stop()

        # 2. Simpan konfigurasi terakhir
        try:
            self.save_config_now()
        except Exception as e:
            rinka_logger.warning(f"Gagal save config saat shutdown: {e}")

        # 3. Hentikan pipeline audio WASAPI
        try:
            self.pipeline_a.stop_stream()
            self.pipeline_b.stop_stream()
        except Exception as e:
            rinka_logger.warning(f"Gagal stop stream saat shutdown: {e}")

        # 4. Sembunyikan system tray
        try:
            if hasattr(self, "tray_icon"):
                self.tray_icon.hide()
        except Exception:
            pass

        # 5. Lepas koneksi PortAudio WASAPI
        try:
            sd._terminate()
        except Exception:
            pass

    def close_application(self):
        self.clean_shutdown()
        QApplication.quit()
        sys.exit(0)

    def reload_application(self):
        reply = QMessageBox.question(
            self, "Restart Aplikasi",
            "Aplikasi akan direstart bersih (audio engine dimatikan, file disimpan, "
            "lalu aplikasi dibuka kembali secara otomatis).\n\nLanjutkan?",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
            QMessageBox.StandardButton.No
        )
        if reply != QMessageBox.StandardButton.Yes:
            return

        self.clean_shutdown()

        try:
            python = sys.executable
            if sys.platform == "win32":
                if getattr(sys, "frozen", False):
                    target = f'"{python}"'
                else:
                    args = " ".join(f'"{arg}"' for arg in sys.argv)
                    target = f'"{python}" {args}'
                # Penundaan ~1.2s memberi waktu OS Windows dan driver audio melepas semua handle
                cmd = f'ping 127.0.0.1 -n 2 >nul & start "" {target}'
                CREATE_NO_WINDOW = 0x08000000
                subprocess.Popen(cmd, shell=True, creationflags=CREATE_NO_WINDOW)
            else:
                time.sleep(0.5)
                subprocess.Popen([python] + sys.argv)
        except Exception as e:
            QMessageBox.critical(
                self, "Reload Gagal",
                f"Tidak bisa memulai ulang aplikasi secara otomatis:\n{e}\n\n"
                "Silakan buka aplikasi kembali secara manual."
            )
            return

        QApplication.quit()
        sys.exit(0)

    def closeEvent(self, event):
        self.clean_shutdown()
        event.accept()
        
    def resizeEvent(self, event):
        super().resizeEvent(event)
        self.trigger_auto_save()

    def moveEvent(self, event):
        super().moveEvent(event)
        self.trigger_auto_save()


# ==============================================================================
# 9. ENTRY POINT
# ==============================================================================
if __name__ == "__main__":
    app = QApplication(sys.argv)
    app.setStyle("Fusion")
    window = RinkaSoundBridgeWindow()
    window.show()
    sys.exit(app.exec())

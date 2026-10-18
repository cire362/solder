"""OS input only. No GPUI test platform, focus override, or app instrumentation.

Bindings follow Xlib/XTEST and Win32 INPUT/SendInput. References:
https://www.x.org/releases/current/doc/libXtst/xtestlib.html
https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-input
https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput
"""

import ctypes as c
import ctypes.util
import os
from pathlib import Path
import subprocess
import time


class Unavailable(RuntimeError):
    pass


def bind(library, name, result, *arguments):
    function = getattr(library, name)
    function.restype = result
    function.argtypes = list(arguments)
    return function


def canonical_path(path):
    """Match std::fs::canonicalize, including Windows' verbatim path prefix.

    https://doc.rust-lang.org/std/fs/fn.canonicalize.html
    https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getfinalpathnamebyhandlew
    """
    if os.name != "nt":
        return Path(path).resolve(strict=True)
    from ctypes import wintypes as w
    kernel = c.WinDLL("kernel32", use_last_error=True)
    opened = bind(kernel, "CreateFileW", w.HANDLE, w.LPCWSTR, w.DWORD, w.DWORD,
                  c.c_void_p, w.DWORD, w.DWORD, w.HANDLE)
    final = bind(kernel, "GetFinalPathNameByHandleW", w.DWORD, w.HANDLE, w.LPWSTR,
                 w.DWORD, w.DWORD)
    closed = bind(kernel, "CloseHandle", w.BOOL, w.HANDLE)
    handle = opened(str(path), 0, 7, None, 3, 0x02000000, None)
    if handle == c.c_void_p(-1).value:
        raise OSError(c.get_last_error(), "CreateFileW cannot canonicalize the fixture path")
    try:
        buffer = c.create_unicode_buffer(32768)
        length = final(handle, buffer, len(buffer), 0)
        if not 0 < length < len(buffer):
            raise OSError(c.get_last_error(), "GetFinalPathNameByHandleW failed")
        return Path(buffer.value)
    finally:
        closed(handle)


class X11:
    backend = "native-x11"

    def __init__(self, env=None):
        env = os.environ if env is None else env
        if env.get("ZED_HEADLESS") is not None:
            raise Unavailable("ZED_HEADLESS selects GPUI's headless backend")
        if env.get("WAYLAND_DISPLAY"):
            raise Unavailable("This harness exercises X11, not Wayland")
        x11 = ctypes.util.find_library("X11")
        xtst = ctypes.util.find_library("Xtst")
        if not x11 or not xtst or not env.get("DISPLAY"):
            raise Unavailable("DISPLAY, libX11 and libXtst are required")
        self.x = c.CDLL(x11)
        self.t = c.CDLL(xtst)
        self.open = bind(self.x, "XOpenDisplay", c.c_void_p, c.c_char_p)
        self.close = bind(self.x, "XCloseDisplay", c.c_int, c.c_void_p)
        self.d = self.open(env["DISPLAY"].encode())
        if not self.d:
            raise Unavailable("XOpenDisplay could not connect to DISPLAY")
        self.root = bind(self.x, "XDefaultRootWindow", c.c_ulong, c.c_void_p)(self.d)
        self.screen = bind(self.x, "XDefaultScreen", c.c_int, c.c_void_p)(self.d)
        self.selection_owner = bind(self.x, "XGetSelectionOwner", c.c_ulong,
                                    c.c_void_p, c.c_ulong)
        self.query = bind(self.x, "XQueryTree", c.c_int, c.c_void_p, c.c_ulong,
                          c.POINTER(c.c_ulong), c.POINTER(c.c_ulong),
                          c.POINTER(c.POINTER(c.c_ulong)), c.POINTER(c.c_uint))
        self.free = bind(self.x, "XFree", c.c_int, c.c_void_p)
        self.atom = bind(self.x, "XInternAtom", c.c_ulong, c.c_void_p, c.c_char_p, c.c_int)
        self.property = bind(self.x, "XGetWindowProperty", c.c_int, c.c_void_p,
                             c.c_ulong, c.c_ulong, c.c_long, c.c_long, c.c_int,
                             c.c_ulong, c.POINTER(c.c_ulong), c.POINTER(c.c_int),
                             c.POINTER(c.c_ulong), c.POINTER(c.c_ulong),
                             c.POINTER(c.POINTER(c.c_ubyte)))
        self.focus = bind(self.x, "XGetInputFocus", c.c_int, c.c_void_p,
                          c.POINTER(c.c_ulong), c.POINTER(c.c_int))
        self.fetch_name = bind(self.x, "XFetchName", c.c_int, c.c_void_p,
                               c.c_ulong, c.POINTER(c.c_void_p))
        self.geometry = bind(self.x, "XGetGeometry", c.c_int, c.c_void_p, c.c_ulong,
                             c.POINTER(c.c_ulong), c.POINTER(c.c_int), c.POINTER(c.c_int),
                             *([c.POINTER(c.c_uint)] * 4))
        self.keysym = bind(self.x, "XStringToKeysym", c.c_ulong, c.c_char_p)
        self.keycode = bind(self.x, "XKeysymToKeycode", c.c_ubyte, c.c_void_p, c.c_ulong)
        self.fake = bind(self.t, "XTestFakeKeyEvent", c.c_int, c.c_void_p,
                         c.c_uint, c.c_int, c.c_ulong)
        self.sync = bind(self.x, "XSync", c.c_int, c.c_void_p, c.c_int)
        query_extension = bind(self.t, "XTestQueryExtension", c.c_int, c.c_void_p,
                               *([c.POINTER(c.c_int)] * 4))
        values = [c.c_int() for _ in range(4)]
        if not query_extension(self.d, *(c.byref(v) for v in values)):
            self.close(self.d)
            raise Unavailable("The X server has no XTEST extension")

    def children(self, window):
        root, parent, count = c.c_ulong(), c.c_ulong(), c.c_uint()
        children = c.POINTER(c.c_ulong)()
        if not self.query(self.d, window, c.byref(root), c.byref(parent),
                          c.byref(children), c.byref(count)):
            return []
        try:
            return list(children[:count.value])
        finally:
            if children:
                self.free(children)

    def pid(self, window):
        kind, fmt, size, after = c.c_ulong(), c.c_int(), c.c_ulong(), c.c_ulong()
        data = c.POINTER(c.c_ubyte)()
        atom = self.atom(self.d, b"_NET_WM_PID", 1)
        if not atom:
            return None
        status = self.property(self.d, window, atom, 0, 1, 0, 0,
                               c.byref(kind), c.byref(fmt), c.byref(size),
                               c.byref(after), c.byref(data))
        try:
            if status == 0 and fmt.value == 32 and size.value == 1 and data:
                return c.cast(data, c.POINTER(c.c_ulong))[0]
        finally:
            if data:
                self.free(data)
        return None

    def windows(self, pid):
        # A window manager may reparent the app inside a frame.
        candidates = self.children(self.root)
        candidates += [child for frame in candidates for child in self.children(frame)]
        return [window for window in candidates if self.pid(window) == pid]

    def foreground(self):
        window, revert = c.c_ulong(), c.c_int()
        self.focus(self.d, c.byref(window), c.byref(revert))
        return window.value

    def window_manager(self):
        # ICCCM managers own WM_Sn for their screen. Observing it never changes focus.
        atom = self.atom(self.d, f"WM_S{self.screen}".encode("ascii"), 1)
        return (self.selection_owner(self.d, atom) or None) if atom else None

    def owns_focus(self, window, pid):
        focused = self.foreground()
        return focused == window or focused in self.children(window)

    def save_dialog(self, pid):
        window = self.foreground()
        if window in (0, 1) or self.pid(window) == pid:
            return None
        name = c.c_void_p()
        if not self.fetch_name(self.d, window, c.byref(name)) or not name:
            return None
        try:
            title = c.string_at(name).decode("utf-8", errors="replace")
            return window if title in ("Save File", "Save", "Save As", "Save a File") else None
        finally:
            self.free(name)

    def event(self, name, pressed):
        code = self.keycode(self.d, self.keysym(name.encode("ascii")))
        if not code or not self.fake(self.d, code, int(pressed), 0):
            raise Unavailable(f"XTEST cannot send {name}")
        self.sync(self.d, 0)

    def chord(self, *names):
        mapped = {"CTRL": "Control_L", "ALT": "Alt_L", "SHIFT": "Shift_L",
                  "ENTER": "Return", "ESC": "Escape", "HOME": "Home", "END": "End"}
        keys = [mapped.get(name, name) for name in names]
        for key in keys:
            self.event(key, True)
        for key in reversed(keys):
            self.event(key, False)
        time.sleep(0.12)

    def text(self, text):
        # The harness uses only ASCII; XTEST must use the server's real keymap.
        special = {" ": "space", "\n": "Return", "#": "numbersign",
                   "_": "underscore", "-": "minus", ".": "period"}
        for char in text:
            name = special.get(char, char)
            shifted = char in "#_" or char.isupper()
            if shifted:
                self.event("Shift_L", True)
            self.event(name, True)
            self.event(name, False)
            if shifted:
                self.event("Shift_L", False)
            time.sleep(0.01)

    def screenshot(self, path, window):
        root, x, y = c.c_ulong(), c.c_int(), c.c_int()
        width, height, border, depth = [c.c_uint() for _ in range(4)]
        if not self.geometry(self.d, window, c.byref(root), c.byref(x), c.byref(y),
                             c.byref(width), c.byref(height), c.byref(border), c.byref(depth)):
            raise Unavailable("XGetGeometry failed")
        get_image = bind(self.x, "XGetImage", c.c_void_p, c.c_void_p, c.c_ulong,
                         c.c_int, c.c_int, c.c_uint, c.c_uint, c.c_ulong, c.c_int)
        get_pixel = bind(self.x, "XGetPixel", c.c_ulong, c.c_void_p, c.c_int, c.c_int)
        destroy = bind(self.x, "XDestroyImage", c.c_int, c.c_void_p)
        image = get_image(self.d, window, 0, 0, width.value, height.value,
                          c.c_ulong(-1).value, 2)  # AllPlanes, ZPixmap
        if not image:
            raise Unavailable("XGetImage failed")
        try:
            # Xvfb is started with a 24-bit TrueColor display. Refuse other visuals.
            if depth.value not in (24, 32):
                raise Unavailable(f"Unsupported screenshot depth {depth.value}")
            pixels = bytearray()
            for row in range(height.value):
                for column in range(width.value):
                    value = get_pixel(image, column, row)
                    pixels.extend(((value >> 16) & 255, (value >> 8) & 255, value & 255))
            target = Path(path).with_suffix(".ppm")
            target.write_bytes(f"P6\n{width.value} {height.value}\n255\n".encode() + pixels)
            if len(set(pixels)) < 3:
                raise Unavailable("The OS window capture appears blank")
            return str(target)
        finally:
            destroy(image)

    def finish(self):
        self.close(self.d)


class Windows:
    backend = "native-win32"

    def __init__(self):
        from ctypes import wintypes as w

        self.w = w
        self.u = c.WinDLL("user32", use_last_error=True)
        # The full union is needed: INPUT is 40 bytes on x64, not KEYBDINPUT's size.
        class Keyboard(c.Structure):
            _fields_ = [("vk", w.WORD), ("scan", w.WORD), ("flags", w.DWORD),
                        ("time", w.DWORD), ("extra", c.c_size_t)]

        class Mouse(c.Structure):
            _fields_ = [("x", w.LONG), ("y", w.LONG), ("data", w.DWORD),
                        ("flags", w.DWORD), ("time", w.DWORD), ("extra", c.c_size_t)]

        class Hardware(c.Structure):
            _fields_ = [("message", w.DWORD), ("low", w.WORD), ("high", w.WORD)]

        class Union(c.Union):
            _fields_ = [("keyboard", Keyboard), ("mouse", Mouse), ("hardware", Hardware)]

        class Input(c.Structure):
            _fields_ = [("type", w.DWORD), ("value", Union)]

        self.Keyboard, self.Union, self.Input = Keyboard, Union, Input
        self.send = bind(self.u, "SendInput", w.UINT, w.UINT, c.POINTER(Input), c.c_int)
        self.front = bind(self.u, "GetForegroundWindow", w.HWND)
        self.process = bind(self.u, "GetWindowThreadProcessId", w.DWORD, w.HWND,
                            c.POINTER(w.DWORD))
        self.visible = bind(self.u, "IsWindowVisible", w.BOOL, w.HWND)
        self.class_name = bind(self.u, "GetClassNameW", c.c_int, w.HWND, w.LPWSTR, c.c_int)
        self.callback = c.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)
        self.enum = bind(self.u, "EnumWindows", w.BOOL, self.callback, w.LPARAM)
        opened = bind(self.u, "OpenInputDesktop", w.HANDLE, w.DWORD, w.BOOL, w.DWORD)
        closed = bind(self.u, "CloseDesktop", w.BOOL, w.HANDLE)
        desktop = opened(0, False, 1)  # DESKTOP_READOBJECTS
        if not desktop:
            raise Unavailable(f"No accessible input desktop (Win32 {c.get_last_error()})")
        closed(desktop)
        if c.sizeof(Input) != (40 if c.sizeof(c.c_void_p) == 8 else 28):
            raise RuntimeError("Incorrect Win32 INPUT ABI")
        # Keeps screenshots in physical pixels on runners using display scaling.
        bind(self.u, "SetProcessDPIAware", w.BOOL)()

    def pid(self, window):
        pid = self.w.DWORD()
        self.process(window, c.byref(pid))
        return pid.value

    def windows(self, pid):
        found = []
        @self.callback
        def callback(window, _):
            if self.visible(window) and self.pid(window) == pid:
                found.append(window)
            return True
        self.enum(callback, 0)
        return found

    def foreground(self):
        return self.front()

    def owns_focus(self, window, pid):
        return self.foreground() == window and self.pid(window) == pid

    def save_dialog(self, pid):
        window = self.foreground()
        name = c.create_unicode_buffer(256)
        self.class_name(window, name, len(name))
        return window if self.pid(window) == pid and name.value == "#32770" else None

    def event(self, vk=0, scan=0, flags=0):
        value = self.Input(1, self.Union(keyboard=self.Keyboard(vk, scan, flags, 0, 0)))
        if self.send(1, c.byref(value), c.sizeof(value)) != 1:
            raise Unavailable(f"SendInput failed (Win32 {c.get_last_error()})")

    def chord(self, *names):
        mapped = {"CTRL": 0x11, "ALT": 0x12, "SHIFT": 0x10, "ENTER": 0x0D,
                  "ESC": 0x1B, "HOME": 0x24, "END": 0x23}
        keys = [mapped[name] if name in mapped else ord(name.upper()) for name in names]
        for key in keys:
            self.event(vk=key)
        for key in reversed(keys):
            self.event(vk=key, flags=2)
        time.sleep(0.12)

    def text(self, text):
        for char in text:
            if char == "\n":
                self.chord("ENTER")
            else:
                self.event(scan=ord(char), flags=4)  # KEYEVENTF_UNICODE
                self.event(scan=ord(char), flags=6)
            time.sleep(0.01)

    def screenshot(self, path, window):
        target = Path(path).with_suffix(".png")
        env = os.environ.copy()
        env["SOLDER_NATIVE_SCREENSHOT"] = str(target)
        command = """
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$bounds = [System.Windows.Forms.SystemInformation]::VirtualScreen
$image = New-Object System.Drawing.Bitmap $bounds.Width,$bounds.Height
$graphics = [System.Drawing.Graphics]::FromImage($image)
try {
  $graphics.CopyFromScreen($bounds.Left,$bounds.Top,0,0,$bounds.Size)
  $image.Save($env:SOLDER_NATIVE_SCREENSHOT,[System.Drawing.Imaging.ImageFormat]::Png)
} finally { $graphics.Dispose(); $image.Dispose() }
"""
        result = subprocess.run(["powershell", "-NoProfile", "-NonInteractive", "-Command", command],
                                env=env, capture_output=True, text=True, timeout=30)
        if result.returncode or not target.exists():
            raise Unavailable("OS screenshot failed: " + result.stderr)
        return str(target)

    def finish(self):
        pass

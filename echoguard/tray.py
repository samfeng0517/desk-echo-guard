"""System-tray icon (pystray) so closing the window keeps the program running.

pystray runs its own thread; menu clicks are forwarded as plain strings
("show", "toggle", "quit") through ``post`` and handled on the Tk thread.
"""

from __future__ import annotations


class Tray:
    def __init__(self, icon):
        self._icon = icon
        self.running = False

    @classmethod
    def create(cls, png_path: str, post) -> "Tray | None":
        """None when pystray / Pillow are missing or no tray is available."""
        try:
            import pystray
            from PIL import Image
        except Exception:
            return None
        holder: dict = {}

        def label(_item):
            return "■ 停止" if holder["tray"].running else "▶ 開始"

        menu = pystray.Menu(
            pystray.MenuItem("顯示視窗", lambda: post("show"), default=True),
            pystray.MenuItem(label, lambda: post("toggle")),
            pystray.Menu.SEPARATOR,
            pystray.MenuItem("結束", lambda: post("quit")),
        )
        try:
            image = Image.open(png_path)
            icon = pystray.Icon("DeskEchoGuard", image, "Desk Echo Guard（已停止）", menu)
            tray = cls(icon)
            holder["tray"] = tray
            icon.run_detached()
        except Exception:
            return None
        return tray

    def set_running(self, running: bool) -> None:
        self.running = running
        try:
            self._icon.title = "Desk Echo Guard（執行中）" if running else "Desk Echo Guard（已停止）"
            self._icon.update_menu()
        except Exception:
            pass

    def notify(self, message: str) -> None:
        try:
            self._icon.notify(message, "Desk Echo Guard")
        except Exception:
            pass

    def stop(self) -> None:
        try:
            self._icon.stop()
        except Exception:
            pass

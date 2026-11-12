#!/usr/bin/env python3
"""Render synthetic playback in Linux's WebKitGTK, without speaker output.

Requires python3-gi, gir1.2-webkit2-4.1 and a display (or xvfb-run).
"""
import json
from pathlib import Path
import sys

import gi

gi.require_version("Gtk", "3.0")
gi.require_version("WebKit2", "4.1")
from gi.repository import GLib, Gtk, WebKit2

if not Gtk.init_check()[0]:
    sys.exit("A display is required; run under xvfb-run on a headless host.")

exit_code = 1
manager = WebKit2.UserContentManager()
manager.register_script_message_handler("audioTest")


def completed(_manager, message):
    global exit_code
    result = json.loads(message.get_js_value().to_string())
    print(json.dumps(result, indent=2), flush=True)
    exit_code = 0 if result["ok"] else 1
    Gtk.main_quit()


def timed_out():
    print("WebKit audio rendering timed out", file=sys.stderr)
    Gtk.main_quit()
    return False


manager.connect("script-message-received::audioTest", completed)
view = WebKit2.WebView(user_content_manager=manager)
view.get_settings().set_allow_file_access_from_file_urls(True)
window = Gtk.OffscreenWindow()
window.add(view)
window.show_all()
page = Path(__file__).resolve().parent.parent / "tests/audio/playback-render.html"
view.load_uri(page.as_uri())
GLib.timeout_add_seconds(30, timed_out)
Gtk.main()
window.destroy()
sys.exit(exit_code)

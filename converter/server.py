"""Pinakes' document converter: turns an uploaded file into Markdown with Microsoft's MarkItDown.

One endpoint, `POST /convert`: the request body is the file's bytes and the `X-File-Extension`
header says what it is (`docx`, `pdf`, ...). The answer is `{"markdown", "converter",
"version"}`. Only the bytes are converted: no paths, no URLs, no plugins, so an upload can't
make the converter read files or fetch anything. `GET /health` says which converter runs.

Errors: 415 for a format it doesn't take, 413 for a file over CONVERT_MAX_BYTES, 422 when the
file can't be converted, 504 when it takes longer than CONVERT_TIMEOUT_SECS.
"""

import io
import json
import os
import signal
from http.server import BaseHTTPRequestHandler, HTTPServer
from importlib.metadata import version

from markitdown import MarkItDown, StreamInfo

PORT = int(os.environ.get("CONVERT_PORT", "8090"))
MAX_BYTES = int(os.environ.get("CONVERT_MAX_BYTES", str(20 * 1024 * 1024)))
TIMEOUT_SECS = int(os.environ.get("CONVERT_TIMEOUT_SECS", "60"))
FORMATS = {"docx", "pptx", "xlsx", "xls", "pdf", "html", "htm"}
VERSION = version("markitdown")

converter = MarkItDown(enable_plugins=False)


class TookTooLong(BaseException):
    # Not an Exception: MarkItDown catches those per converter and would carry on.
    pass


def on_alarm(_signum, _frame):
    raise TookTooLong()


signal.signal(signal.SIGALRM, on_alarm)


class Handler(BaseHTTPRequestHandler):
    def reply(self, status, body):
        data = json.dumps(body, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path != "/health":
            return self.reply(404, {"error": "not found"})
        self.reply(200, {"converter": "markitdown", "version": VERSION})

    def do_POST(self):
        if self.path != "/convert":
            return self.reply(404, {"error": "not found"})
        # The body is read before any answer: closing on an unread body resets the connection,
        # and the caller would see that instead of the answer. (The service caps uploads.)
        length = int(self.headers.get("Content-Length") or 0)
        data = self.rfile.read(length) if length > 0 else b""
        ext = self.headers.get("X-File-Extension", "").strip().lower().lstrip(".")
        if ext not in FORMATS:
            return self.reply(415, {"error": f"unsupported format: {ext or 'none given'}"})
        if not data:
            return self.reply(400, {"error": "the body is the file, and it is empty"})
        if length > MAX_BYTES:
            return self.reply(413, {"error": f"the file is larger than {MAX_BYTES} bytes"})
        # One request at a time (HTTPServer is single-threaded), so the alarm is this one's.
        signal.alarm(TIMEOUT_SECS)
        try:
            result = converter.convert_stream(
                io.BytesIO(data), stream_info=StreamInfo(extension="." + ext)
            )
        except TookTooLong:
            return self.reply(504, {"error": f"conversion took over {TIMEOUT_SECS} s"})
        except Exception as e:  # what MarkItDown raises varies by format
            return self.reply(422, {"error": f"could not convert the file: {type(e).__name__}"})
        finally:
            signal.alarm(0)
        self.reply(
            200,
            {"markdown": result.markdown, "converter": "markitdown", "version": VERSION},
        )

    def log_message(self, fmt, *args):
        # The request line only: never file contents.
        print(f"{self.address_string()} {fmt % args}", flush=True)


if __name__ == "__main__":
    print(f"markitdown {VERSION} converting on port {PORT}", flush=True)
    HTTPServer(("0.0.0.0", PORT), Handler).serve_forever()

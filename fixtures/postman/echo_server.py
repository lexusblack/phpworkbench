#!/usr/bin/env python3
"""Tiny echo server used to verify php-http request bodies."""
import argparse
import json
import re
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs


def parse_multipart(raw, content_type):
    match = re.search(r'boundary="?([^";]+)"?', content_type)
    if not match:
        return {}, {}
    boundary = match.group(1).encode()
    fields, files = {}, {}
    for part in raw.split(b"--" + boundary):
        if not part.strip() or part.strip() == b"--":
            continue
        head, _, body = part.partition(b"\r\n\r\n")
        headers = head.decode("utf-8", "replace")
        name_match = re.search(r'name="([^"]*)"', headers)
        if not name_match:
            continue
        name = name_match.group(1)
        value = body.rstrip(b"\r\n").decode("utf-8", "replace")
        if "filename=" in headers:
            filename_match = re.search(r'filename="([^"]*)"', headers)
            files[name] = {
                "present": True,
                "filename": filename_match.group(1) if filename_match else "",
                "content": value,
            }
        else:
            fields[name] = value
    return fields, files


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        self.echo()

    def do_POST(self):
        self.echo()

    def echo(self):
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length)
        content_type = self.headers.get("Content-Type", "")
        payload = {
            "method": self.command,
            "content_type": content_type,
            "authorization": self.headers.get("Authorization"),
            "path": self.path,
        }
        if content_type.startswith("application/x-www-form-urlencoded"):
            payload["form"] = parse_qs(raw.decode("utf-8"), keep_blank_values=True)
        elif content_type.startswith("multipart/form-data"):
            fields, files = parse_multipart(raw, content_type)
            payload["form"] = fields
            payload["files"] = files
        elif content_type.startswith("application/json"):
            payload["json"] = json.loads(raw.decode("utf-8")) if raw else None

        body = json.dumps(payload, indent=2, sort_keys=True).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=8765)
    args = parser.parse_args()
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    server.serve_forever()


if __name__ == "__main__":
    main()

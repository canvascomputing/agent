"""The browser feed behind the Apparat Fabrik: one frame per event.

Frames are fanned out over server-sent events to every page connected, and
this file holds the small server that carries them, so the example beside it
is about the shift rather than about HTTP. A run can record its frames, and
`replay` pushes that file back out at the speed it happened, which shows a run
again without calling a model.
"""

import asyncio
import json
import time
import webbrowser

HOST = "127.0.0.1"

# What a frame carries beyond the event's own name. Tool input, tool output and
# reply chunks are left out: they run to megabytes over one run, and no page
# draws them.
CARRIED = (
    "tool_name",
    "action",
    "slug",
    "model",
    "usage",
    "reason",
    "config",
    "limit",
    "attempt",
)


class Feed:
    """The frames a page draws, fanned out to every browser connected."""

    def __init__(self, record_file=None):
        self.history = []
        self.clients = set()
        self.opened = asyncio.Event()
        self.record = record_file.open("w") if record_file else None

    def push(self, frame):
        frame["n"] = len(self.history)
        self.history.append(frame)
        for client in self.clients:
            client.put_nowait(frame)
        if self.record:
            self.record.write(json.dumps(frame) + "\n")
            self.record.flush()

    def subscribe(self):
        client = asyncio.Queue()
        for frame in self.history:
            client.put_nowait(frame)
        self.clients.add(client)
        self.opened.set()
        return client

    def unsubscribe(self, client):
        self.clients.discard(client)


def where(arguments, event_data, root):
    """The place a call is about: the file it opened or the path it searched."""
    place = event_data.get("path")
    if not place and isinstance(arguments, dict):
        place = arguments.get("path")
    if not isinstance(place, str):
        return None
    prefix = f"{root}/"
    return place[len(prefix) :] if place.startswith(prefix) else place


def detail(arguments):
    """The one field of a call worth showing: what was searched or read."""
    if not isinstance(arguments, dict):
        return None
    for key in ("pattern", "query", "path", "slug", "action", "command"):
        value = arguments.get(key)
        if isinstance(value, str):
            # A path is only recognisable by its tail, and a page draws the
            # label across its width, so an absolute one is cut to two parts.
            if "/" in value:
                value = "/".join(value.rsplit("/", 2)[-2:])
            return value[:40]
    return None


def frame_for(event, started_at, root=""):
    """One event, cut down to what a page can draw."""
    data = {key: value for key, value in event.data.items() if key in CARRIED}
    message = event.data.get("message")
    if isinstance(message, str):
        data["message"] = message[:120]
    arguments = event.data.get("input")
    said = detail(arguments)
    if said:
        data["detail"] = said
    place = where(arguments, event.data, root)
    if place:
        data["place"] = place
    return {
        "t": time.monotonic() - started_at,
        "kind": event.kind,
        "agent": event.agent_id,
        "task": event.task_key,
        "data": data,
    }


def response(status, content_type, length):
    reason = "OK" if status == 200 else "Not Found"
    return (
        f"HTTP/1.1 {status} {reason}\r\n"
        f"Content-Type: {content_type}\r\n"
        f"Content-Length: {length}\r\n"
        "Connection: close\r\n\r\n"
    ).encode()


async def stream(writer, feed):
    writer.write(
        b"HTTP/1.1 200 OK\r\n"
        b"Content-Type: text/event-stream\r\n"
        b"Cache-Control: no-cache\r\n"
        b"Connection: keep-alive\r\n\r\n"
    )
    client = feed.subscribe()
    try:
        while True:
            frame = await client.get()
            writer.write(b"data: " + json.dumps(frame).encode() + b"\n\n")
            await writer.drain()
    finally:
        feed.unsubscribe(client)


async def serve(reader, writer, feed, page):
    """One request: the page itself, or the event stream it reads."""
    try:
        request = await reader.readline()
        while (await reader.readline()) not in (b"\r\n", b"\n", b""):
            pass
        parts = request.decode("latin-1").split(" ")
        path = parts[1].split("?")[0] if len(parts) > 1 else "/"
        if path == "/":
            body = page.read_bytes()
            writer.write(response(200, "text/html; charset=utf-8", len(body)))
            writer.write(body)
            await writer.drain()
        elif path == "/events":
            await stream(writer, feed)
        else:
            writer.write(response(404, "text/plain", 0))
            await writer.drain()
    except (ConnectionResetError, BrokenPipeError, asyncio.CancelledError):
        pass
    finally:
        writer.close()


async def open_view(feed, page, port):
    """Serve the page, open it, and hold until it is watching."""
    server = await asyncio.start_server(lambda r, w: serve(r, w, feed, page), HOST, port)
    url = f"http://{HOST}:{port}"
    print(f"{page.stem}: {url}", flush=True)
    webbrowser.open(url)
    try:
        await asyncio.wait_for(feed.opened.wait(), timeout=60)
    except asyncio.TimeoutError:
        print("no browser connected, starting anyway", flush=True)
    return server, url


async def replay(feed, frames_file, speed=1.0):
    """Push a recorded run back out at the speed it happened.

    `speed` below one plays it slower, which is what a screen recording wants:
    a capture of a few frames a second then samples movement that has not
    jumped far between one frame and the next.
    """
    frames = [json.loads(line) for line in frames_file.read_text().splitlines() if line]
    started_at = time.monotonic()
    for frame in frames:
        # A quiet stretch is dead air on screen, so the longest wait is two
        # seconds however long the run paused there.
        wait = frame.get("t", 0) / speed - (time.monotonic() - started_at)
        if wait > 0:
            await asyncio.sleep(min(wait, 2.0))
        feed.push(frame)


async def watch_pages(notes, feed, started_at):
    """Poll the store: an event says a page was written, never which one."""
    seen = set()
    while True:
        for page in notes.pages().list():
            if page.slug in seen:
                continue
            seen.add(page.slug)
            feed.push(
                {
                    "t": time.monotonic() - started_at,
                    "kind": "page",
                    "slug": page.slug,
                    "description": page.description,
                }
            )
        await asyncio.sleep(1)

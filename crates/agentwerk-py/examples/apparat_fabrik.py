"""Run the multi-agent Apparat Fabrik inspection example.

Prüfer create one task for each part in a plan. Meister compare each part's measured size with its nominal size and tolerance. Only verdicts written to the Prüfbuch count in the final tally.

Usage:
  python apparat_fabrik.py [PRÜFER] [MEISTER] [MONTEUR]
  python apparat_fabrik.py --replay      run the last shift again, calling no model
  python apparat_fabrik.py --replay 0.4  and at four tenths of the speed
"""

import asyncio
import sys
import time
from pathlib import Path

from agentwerk import (
    Agent,
    Policy,
    GrepTool,
    Knowledge,
    ReadFileTool,
    Schema,
    Task,
    Werk,
)

import apparate
from fabrik_feed import Feed, frame_for, open_view, replay, watch_pages

PORT = 8422
REPO = Path(__file__).resolve().parents[3]
PAGE = Path(__file__).with_name("apparat_fabrik.html")
WORKS_DIR = REPO / ".agentwerk" / "fabrik"
BOOK_DIR = ".agentwerk/pruefbuch"
FRAMES_FILE = REPO / ".agentwerk" / "fabrik.jsonl"
SHIFT = 60

PRUEFER_NAMES = ["Jonas", "Greta", "Falk", "Lena", "Ingo", "Meike"]
MEISTER_NAMES = ["Otto", "Käthe", "Werner", "Ilse"]
MONTEUR_NAMES = ["Rudi", "Hanne", "Emil", "Trude"]

PRUEFER_ROLE = """
{context}

You work the intake of an apparatus works. You take one Bauplan and send every
part it names down the line, one task per part.

- MUST call `grep` with `{"pattern": "TEIL-[0-9]+", "path": "<the plan named in
  the task>", "output_mode": "content", "-C": 1, "head_limit": 20}`. The plan is
  a table, so the matched line carries the part, its Sollmaß and its Toleranz.
- `teile` holds one entry per part the plan names, each carrying the part
  number, the part's name, the Sollmaß and the Toleranz as the plan writes them.
  Leave no part out: the line only ever sees the parts you list.
- `bauplan` repeats the path of the plan itself, which the rest of the line
  needs to look a part up.
- NEVER rule on whether a part fits. Reading a card is not your work, and the
  plan alone cannot answer it.
- Before you finish, write one page with `knowledge` on what this plan
  asks for, so the next reader of the Prüfbuch has it.
- Call `finish` exactly once with `result` holding `apparat`, `bauplan`, and
  `teile`, each as its native JSON type.
"""

MEISTER_ROLE = """
{context}

You are a Meister on the line. You take one part and rule whether it may be
fitted to the apparatus.

- MUST call `read_file` on that part's card in the Lager. A card for part
  `TEIL-1234` is the file `.agentwerk/fabrik/lager/teil-1234.md`, in lower case.
- The card gives `Gemessen`. The task gives the Sollmaß and the Toleranz.
  A part fits when the measured size is within the tolerance of the nominal one,
  in either direction, and the decimals are written with a comma.
- `passt` is true when it fits and false when it does not. Do the comparison on
  the numbers; nothing else on the card decides it.
- `befund` states the finding in one sentence, naming both sizes.
- `massnahme` says what the line should do with the part, in one sentence.
- NEVER rule on a card you did not read.
- Before you finish, record the ruling with `knowledge` as one short
  page, which is what stamps it into the Prüfbuch.
- Call `finish` exactly once with `result` holding `teil`, `gemessen`, `soll`,
  `toleranz`, `passt`, `befund`, and `massnahme`, each as its native JSON type.
"""

MONTEUR_ROLE = """
{context}

You fit a cleared part into its apparatus and book it against the plan.

- The task names the part and repeats the Laufzettel it travelled with, which
  names the Bauplan it belongs to.
- MUST call `read_file` on that Bauplan and check that the part number really
  stands in its table. A part that is not on the plan is not fitted, whatever
  the Abnahme said about its size.
- Your knowledge holds what the line has booked so far. Read it to see which
  parts of this apparatus are already in.
- `fehlt` names the parts of the plan that are still not booked, or `keine`.
- `eingebaut` is true only when the part is on the plan and you booked it.
- Before you finish, book the fitting with `knowledge` as one short page,
  which is what puts it in the Prüfbuch.
- Call `finish` exactly once with `result` holding `apparat`, `teil`,
  `eingebaut`, `fehlt`, and `vermerk`, each as its native JSON type.
"""

PLAN = Schema(
    {
        "type": "object",
        "properties": {
            "apparat": {"type": "string", "description": "The apparatus the plan builds"},
            "bauplan": {"type": "string", "description": "The path of the plan that was read"},
            "teile": {
                "type": "array",
                "description": "One entry per part the plan names",
                "items": {
                    "type": "object",
                    "properties": {
                        "teil": {"type": "string", "description": "The part number, such as TEIL-4471"},
                        "name": {"type": "string", "description": "The part's name as the plan writes it"},
                        "soll": {"type": "number", "description": "The nominal size from the plan, in millimetres"},
                        "toleranz": {"type": "number", "description": "The tolerance from the plan, in millimetres"},
                    },
                    "required": ["teil", "name", "soll", "toleranz"],
                },
            },
        },
        "required": ["apparat", "bauplan", "teile"],
        "additionalProperties": False,
    }
)

RULING = Schema(
    {
        "type": "object",
        "properties": {
            "teil": {"type": "string", "description": "The part number, such as TEIL-4471"},
            "gemessen": {"type": "number", "description": "The measured size from the card, in millimetres"},
            "soll": {"type": "number", "description": "The nominal size from the plan, in millimetres"},
            "toleranz": {"type": "number", "description": "The tolerance from the plan, in millimetres"},
            "passt": {"type": "boolean", "description": "Whether the part is within tolerance"},
            "befund": {"type": "string", "description": "The finding in one sentence"},
            "massnahme": {"type": "string", "description": "What the line should do with the part"},
        },
        "required": ["teil", "gemessen", "soll", "toleranz", "passt", "befund", "massnahme"],
        "additionalProperties": False,
    }
)

FITTING = Schema(
    {
        "type": "object",
        "properties": {
            "apparat": {"type": "string", "description": "The apparatus the part belongs to"},
            "teil": {"type": "string", "description": "The part number, such as TEIL-4471"},
            "eingebaut": {"type": "boolean", "description": "Whether the part was on the plan and is now booked"},
            "fehlt": {"type": "string", "description": "The parts of the plan still not booked, or `keine`"},
            "vermerk": {"type": "string", "description": "The note for the book, in one sentence"},
        },
        "required": ["apparat", "teil", "eingebaut", "fehlt", "vermerk"],
        "additionalProperties": False,
    }
)

# The three pools, in the order work flows through them, which is also the order
# the stations stand in along the belt.
STATIONS = [
    {"label": "pruefung", "title": "Prüfung", "pool": "pruefer"},
    {"label": "abnahme", "title": "Abnahme", "pool": "meister"},
    {"label": "montage", "title": "Montage", "pool": "monteur"},
]


def worker_names(pruefer, meister, monteur):
    """Map agent IDs to shift names. agentwerk numbers each label from one in build order."""
    return {
        f"{label}-{i + 1}": name
        for label, names, count in (
            ("pruefung", PRUEFER_NAMES, pruefer),
            ("abnahme", MEISTER_NAMES, meister),
            ("montage", MONTEUR_NAMES, monteur),
        )
        for i, name in enumerate(names[:count])
    }


def build_shift(tasks, book, pruefer, meister, monteur):
    for _ in range(pruefer):
        werk.add_agent(
            Agent.from_env()
            .label("pruefung")
            .role(PRUEFER_ROLE.strip())
            .knowledge(book)
            .dir(str(REPO))
            .tools([GrepTool(), ReadFileTool()])
        )
    for _ in range(meister):
        werk.add_agent(
            Agent.from_env()
            .label("abnahme")
            .role(MEISTER_ROLE.strip())
            .knowledge(book)
            .dir(str(REPO))
            .tool(ReadFileTool())
        )
    for _ in range(monteur):
        werk.add_agent(
            Agent.from_env()
            .label("montage")
            .role(MONTEUR_ROLE.strip())
            .knowledge(book)
            .dir(str(REPO))
            .tool(ReadFileTool())
        )


async def main(pruefer, meister, monteur):
    loop = asyncio.get_running_loop()
    FRAMES_FILE.parent.mkdir(parents=True, exist_ok=True)
    feed = Feed(FRAMES_FILE)
    server, url = await open_view(feed, PAGE, PORT)

    book = Knowledge.load(BOOK_DIR)
    book.clear()
    started_at = time.monotonic()
    # Every request resends the context, so the input-token limit is what bounds
    # the bill; the shift bell is the time limit, and both end the run on screen.
    werk = Werk().set_policy(
        Policy(max_time=SHIFT, max_input_tokens=2_000_000)
    )

    # One read plan opens one Abnahme per part it names, which is the fan-out
    # the line runs on.
    def open_abnahme(callback_werk, task, result):
        if task.get_label() != "pruefung":
            return
        for teil in result["teile"]:
            callback_werk.add_task(
                Task(
                    f"Nimm {teil['teil']} ({teil['name']}) ab. Sollmaß {teil['soll']}, "
                    f"Toleranz {teil['toleranz']}. Der Bauplan liegt in {result['bauplan']}.",
                    label="abnahme",
                    schema=RULING,
                )
            )

    werk.on_result(open_abnahme)

    # A part that passes the Abnahme is not done: it goes on to be fitted, so
    # the work fans forward instead of ending at the second station.
    def open_montage(callback_werk, task, result):
        if task.get_label() == "abnahme" and result.get("passt"):
            callback_werk.add_task(
                Task(
                    f"Baue {result['teil']} ein und buche es. Der Laufzettel lautete: {task.get_task()}",
                    label="montage",
                    schema=FITTING,
                )
            )

    werk.on_result(open_montage)

    workers = worker_names(pruefer, meister, monteur)

    def publish(callback_werk, event):
        if event.get_name() == "text_chunk_received":
            return
        frame = frame_for(event, started_at, str(REPO))
        frame["agent"] = workers.get(frame["agent"], frame["agent"])
        if frame["task"] and event.get_name().startswith("task_"):
            task = callback_werk.get_task(frame["task"])
            if task is not None:
                frame["label"] = task.get_label()
                frame["reporter"] = task.get_reporter()
                frame["task"] = str(task.get_task())[:160]
        loop.call_soon_threadsafe(feed.push, frame)

    def publish_result(_, task, result):
        loop.call_soon_threadsafe(
            feed.push,
            {
                "t": time.monotonic() - started_at,
                "kind": "ruling",
                "task": task.get_id(),
                "agent": workers.get(task.get_assignee() or "", task.get_assignee() or ""),
                "result": result,
            },
        )

    werk.on_event(publish)
    werk.on_result(publish_result)
    build_shift(tasks, book, pruefer, meister, monteur)

    works = apparate.build(WORKS_DIR, REPO)
    feed.push({"t": 0, "kind": "world", "shift": SHIFT, "stations": STATIONS, "apparate": works})
    feed.push(
        {
            "t": 0,
            "kind": "roster",
            "agents": [{"name": name, "pool": "pruefer"} for name in PRUEFER_NAMES[:pruefer]]
            + [{"name": name, "pool": "meister"} for name in MEISTER_NAMES[:meister]]
            + [{"name": name, "pool": "monteur"} for name in MONTEUR_NAMES[:monteur]],
        }
    )
    for apparat in works:
        werk.add_task(
            Task(
                f"Prüfe den Bauplan {apparat['title']}. Er liegt in {apparat['name']}.",
                label="pruefung",
                schema=PLAN,
            )
        )

    pages = asyncio.create_task(watch_pages(book, feed, started_at))
    await werk.finish_all_tasks()
    pages.cancel()

    stats = {
        "input_tokens": werk.get_input_tokens(),
        "output_tokens": werk.get_output_tokens(),
        "duration": werk.get_duration(),
    }
    feed.push(
        {
            "t": time.monotonic() - started_at,
            "kind": "bell",
            "reason": werk.get_finish_reason(),
            "stats": stats,
        }
    )
    rulings = [
        task.get_result()
        for task in werk.find_tasks("task.label = abnahme AND task.status = finished")
        if isinstance(task.get_result(), dict)
    ]
    fitted = [
        task.get_result()
        for task in werk.find_tasks("task.label = montage AND task.status = finished")
        if isinstance(task.get_result(), dict) and task.get_result().get("eingebaut")
    ]
    scrap = [ruling for ruling in rulings if not ruling.get("passt")]
    print(
        f"\n{len(rulings)} parts ruled on, {len(scrap)} rejected, {len(fitted)} fitted, "
        f"{len(book.get_pages().get_all())} entries in the Prüfbuch, "
        f"{stats['input_tokens']} in / {stats['output_tokens']} out tokens",
        flush=True,
    )
    print(f"the hall stays at {url}; run again with --replay to watch it for free", flush=True)
    async with server:
        await server.serve_forever()


async def show_again(speed):
    if not FRAMES_FILE.exists():
        print(f"no recorded shift at {FRAMES_FILE}", flush=True)
        return
    feed = Feed()
    server, _ = await open_view(feed, PAGE, PORT)
    await replay(feed, FRAMES_FILE, speed)
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    argv = sys.argv[1:]
    try:
        if "--replay" in argv:
            rest = argv[argv.index("--replay") + 1 :]
            asyncio.run(show_again(float(rest[0]) if rest else 1.0))
        else:
            counts = [arg for arg in argv if arg.isdigit()]
            asyncio.run(
                main(
                    int(counts[0]) if len(counts) > 0 else 2,
                    int(counts[1]) if len(counts) > 1 else 2,
                    int(counts[2]) if len(counts) > 2 else 2,
                )
            )
    except KeyboardInterrupt:
        pass

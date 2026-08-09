"""The works the Apparat Fabrik runs, written to disk before the shift.

Every apparatus gets a Bauplan naming the parts it is built from, each with a
nominal size and a tolerance, and every part gets a card in the Lager saying
what it actually measured. The work over it is real and has a right answer: a
part fits or it does not, and finding out means reading the plan, finding the
card, and comparing two numbers.
"""

import random
import shutil

APPARATE = [
    ("zeitgeber-mk-ii", "Zeitgeber MK II"),
    ("laeutwerk-40", "Läutwerk 40"),
    ("regeltrieb-a", "Regeltrieb A"),
    ("windlade-s", "Windlade S"),
    ("zaehlwerk-7", "Zählwerk 7"),
    ("spannwerk-b", "Spannwerk B"),
]

TEILE = [
    "Zahnrad", "Feder", "Spule", "Ventil", "Gehäuse", "Welle",
    "Lager", "Nocken", "Kolben", "Dichtung", "Klemme", "Anker",
]

WERKSTOFFE = ["Messing", "Federstahl", "Grauguss", "Bronze", "Aluminium", "Hartpapier"]

LIEFERANTEN = ["Ostwerk", "Hüttenbau Süd", "Krämer & Sohn", "Nordapparate", "Vogel-Werke"]

BEMERKUNGEN = [
    "Das Teil kam mit einer Schramme am Rand an.",
    "Die Kiste stand über Nacht im Regen. Das Papier ist gewellt, das Teil trocken.",
    "Zweite Lieferung dieser Woche vom selben Lieferanten.",
    "Der Lieferschein nennt ein anderes Los als der Aufkleber.",
    "Nachgemessen am kalten Teil, wie es die Werksordnung verlangt.",
    "Ein Teil aus dieser Kiste ist schon einmal zurückgegangen.",
    "Sauber verpackt, einzeln in Papier.",
    "Die Kanten sind entgratet, die Bohrung ist mittig.",
]

# The plans are the only place a nominal size is written down, and the cards
# are the only place a measured one is: neither file answers the question alone.
NOMINAL = [4.0, 6.5, 8.0, 12.0, 16.5, 20.0, 24.0, 32.0]
TOLERANCES = [0.02, 0.05, 0.10]


def part_number(dice, taken):
    while True:
        number = f"TEIL-{dice.randint(1000, 9989)}"
        if number not in taken:
            taken.add(number)
            return number


def bauplan(title, parts):
    lines = [
        f"# Bauplan: {title}",
        "",
        "Werk: agentwerk · Blatt 1 von 1 · Freigabe 1958",
        "",
        "The apparatus is built from the parts below. Each part carries a number,",
        "a nominal size (Sollmaß) and the tolerance it is allowed to stray by.",
        "A part outside its tolerance may not be fitted.",
        "",
        "| Teil | Teilenummer | Sollmaß | Toleranz |",
        "|------|-------------|---------|----------|",
    ]
    for name, number, nominal, tolerance in parts:
        lines.append(
            f"| {name} | {number} | {nominal:.2f} mm | ±{tolerance:.2f} mm |".replace(".", ",")
        )
    lines += ["", "Die Karten der einzelnen Teile liegen im Lager.", ""]
    return "\n".join(lines)


def lagerkarte(name, number, measured, dice):
    return "\n".join(
        [
            f"# {number} · {name}",
            "",
            f"Lieferant: {dice.choice(LIEFERANTEN)}",
            f"Werkstoff: {dice.choice(WERKSTOFFE)}",
            f"Eingang: Los {dice.randint(11, 89)}",
            "",
            f"Gemessen: {measured:.2f} mm".replace(".", ","),
            "",
            dice.choice(BEMERKUNGEN),
            "",
        ]
    )


def build(root, base, seed=7):
    """Write the plans and the stock cards, and hand back the apparatus list."""
    shutil.rmtree(root, ignore_errors=True)
    (root / "baupläne").mkdir(parents=True, exist_ok=True)
    (root / "lager").mkdir(parents=True, exist_ok=True)
    dice = random.Random(seed)
    taken = set()
    apparate = []
    for slug, title in APPARATE:
        parts = []
        for name in dice.sample(TEILE, dice.randint(4, 6)):
            number = part_number(dice, taken)
            nominal = dice.choice(NOMINAL)
            tolerance = dice.choice(TOLERANCES)
            # A third of the stock is off: without those the shift has nothing
            # to decide and every card reads the same.
            off = dice.random() < 0.34
            stray = tolerance * (dice.uniform(1.4, 3.0) if off else dice.uniform(0, 0.7))
            measured = nominal + stray * dice.choice([1, -1])
            parts.append((name, number, nominal, tolerance))
            (root / "lager" / f"{number.lower()}.md").write_text(
                lagerkarte(name, number, measured, dice)
            )
        (root / "baupläne" / f"{slug}.md").write_text(bauplan(title, parts))
        apparate.append(
            {
                "name": str((root / "baupläne" / f"{slug}.md").relative_to(base)),
                "title": title,
                "teile": [number for _, number, _, _ in parts],
            }
        )
    return apparate

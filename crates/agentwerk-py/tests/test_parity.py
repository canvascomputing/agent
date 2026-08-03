"""The type stubs against the compiled module.

Every gap this catches shipped at least once: a method lands in `src/` and the
`.pyi` never learns about it, so editors and type checkers deny an API that
works. Parsing the stub with `ast` keeps the check honest without a second
source of truth.
"""

import ast
import inspect
import pathlib

import agentwerk as aw

STUB = pathlib.Path(aw.__file__).with_suffix(".pyi")

# Dunders the interpreter adds to every class, plus the two PyO3 stamps on each
# one and the two more a Python-defined class carries. None of them belong in a
# hand-written stub.
INHERITED = set(dir(object)) | {"__dict__", "__module__", "__weakref__"}


def stub_tree():
    return ast.parse(STUB.read_text())


def stub_top_level_names():
    names = set()
    for node in stub_tree().body:
        if isinstance(node, (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)):
            names.add(node.name)
    return names


def stub_class_members(name):
    for node in stub_tree().body:
        if isinstance(node, ast.ClassDef) and node.name == name:
            members = set()
            for member in node.body:
                if isinstance(member, (ast.FunctionDef, ast.AsyncFunctionDef)):
                    members.add(member.name)
                elif isinstance(member, ast.AnnAssign) and isinstance(
                    member.target, ast.Name
                ):
                    members.add(member.target.id)
            return members
    raise AssertionError(f"{name} is missing from {STUB.name}")


def module_classes():
    return [name for name in aw.__all__ if inspect.isclass(getattr(aw, name))]


def test_every_exported_name_is_declared_in_the_stub():
    assert set(aw.__all__) <= stub_top_level_names()


def test_every_class_member_is_declared_in_the_stub():
    missing = {}
    for name in module_classes():
        live = {m for m in dir(getattr(aw, name)) if m not in INHERITED}
        gap = live - stub_class_members(name)
        if gap:
            missing[name] = sorted(gap)
    assert missing == {}


def test_the_stub_declares_nothing_the_module_lacks():
    extra = {}
    for name in module_classes():
        klass = getattr(aw, name)
        gap = {m for m in stub_class_members(name) if not hasattr(klass, m)}
        if gap:
            extra[name] = sorted(gap)
    assert extra == {}

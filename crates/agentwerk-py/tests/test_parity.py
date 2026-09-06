"""Compare the type stubs with the compiled module.

Missing stub methods hide working APIs from editors and type checkers. Parse the stub with ``ast`` so the test does not require another API inventory.
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


def stub_methods(name):
    for node in stub_tree().body:
        if isinstance(node, ast.ClassDef) and node.name == name:
            return [
                member
                for member in node.body
                if isinstance(member, (ast.FunctionDef, ast.AsyncFunctionDef))
            ]
    raise AssertionError(f"{name} is missing from {STUB.name}")


def stub_parameters(node):
    spec = node.args
    names = [arg.arg for arg in spec.posonlyargs + spec.args]
    if spec.vararg:
        names.append("*" + spec.vararg.arg)
    names += [arg.arg for arg in spec.kwonlyargs]
    if spec.kwarg:
        names.append("**" + spec.kwarg.arg)
    return [name for name in names if name != "self"]


def live_parameters(member):
    names = []
    for parameter in inspect.signature(member).parameters.values():
        if parameter.kind is parameter.VAR_POSITIONAL:
            names.append("*" + parameter.name)
        elif parameter.kind is parameter.VAR_KEYWORD:
            names.append("**" + parameter.name)
        else:
            names.append(parameter.name)
    return [name for name in names if name != "self"]


def test_every_exported_name_is_declared_in_the_stub():
    assert set(aw.__all__) == stub_top_level_names()


def test_removed_api_names_are_absent_from_runtime_exports_and_stub():
    removed = {"TasksTool", "FetchUrlTool"}
    assert removed.isdisjoint(aw.__all__)
    assert removed.isdisjoint(stub_top_level_names())
    for name in ("Agent", "Werk"):
        assert "finish_all_tasks" not in stub_class_members(name)
        assert not hasattr(getattr(aw, name), "finish_all_tasks")
    assert "task" not in stub_class_members("Agent")
    assert "handover" not in stub_class_members("Agent")
    assert "get_parent" not in stub_class_members("Task")


def test_every_class_member_is_declared_in_the_stub():
    missing = {}
    for name in module_classes():
        live = {m for m in dir(getattr(aw, name)) if m not in INHERITED}
        gap = live - stub_class_members(name)
        if gap:
            missing[name] = sorted(gap)
    assert missing == {}


def test_every_stub_parameter_is_named_as_the_module_names_it():
    renamed = {}
    for name in module_classes():
        klass = getattr(aw, name)
        for method in stub_methods(name):
            live = klass if method.name == "__init__" else getattr(klass, method.name)
            try:
                expected = live_parameters(live)
            except (TypeError, ValueError):
                # A property and a slot wrapper carry no signature to compare.
                continue
            if stub_parameters(method) != expected:
                renamed[f"{name}.{method.name}"] = (stub_parameters(method), expected)
    assert renamed == {}


def test_the_stub_declares_nothing_the_module_lacks():
    extra = {}
    for name in module_classes():
        klass = getattr(aw, name)
        gap = {m for m in stub_class_members(name) if not hasattr(klass, m)}
        if gap:
            extra[name] = sorted(gap)
    assert extra == {}


def test_agent_and_werk_declare_the_same_async_finish_signatures():
    signatures = []
    for name in ("Agent", "Werk"):
        methods = {method.name: method for method in stub_methods(name)}
        finish = {}
        for method_name in ("finish_task", "finish_tasks", "finish"):
            method = methods[method_name]
            assert isinstance(method, ast.AsyncFunctionDef)
            finish[method_name] = (ast.dump(method.args), ast.dump(method.returns))
        signatures.append(finish)
    assert signatures[0] == signatures[1]

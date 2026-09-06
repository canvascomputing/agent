#!/bin/sh
set -eu

legacy='finish_all_tasks|TasksTool|TasksArgs|FetchUrlTool|FetchUrlArgs|fetch_url|Agent::task|Agent\.task|Agent::handover|Agent\.handover|\.handover\(|get_parent|task\.parent_id|HANDOVER_|TaskMissing|PageMissing|UnknownField|UnknownStatus|InvalidTime|RepeatedField|UnexpectedToken|UnexpectedEnd|set_char_limit'
if rg -n "$legacy" \
    --glob '!target/**' \
    --glob '!.git/**' \
    --glob '!crates/agentwerk-py/tests/test_agent.py' \
    --glob '!crates/agentwerk-py/tests/test_tools.py' \
    --glob '!crates/agentwerk-py/tests/test_parity.py' \
    --glob '!tools/check-names.sh' \
    .; then
    echo 'legacy API name found' >&2
    exit 1
fi

if rg -n 'Tool::new\("(tasks|fetch_url)"\)' crates README.md agentdocs INVENTORY.md; then
    echo 'legacy model-facing tool name found' >&2
    exit 1
fi

missing=0
for file in $(find crates/agentwerk/src crates/agentwerk-py/src \
    -name '*.rs' ! -name '*_tests.rs' ! -name 'test_util.rs' | sort); do
    if ! rg -q "^## \`$file\`$" INVENTORY.md; then
        echo "INVENTORY.md is missing $file" >&2
        missing=1
    fi
done
test "$missing" -eq 0

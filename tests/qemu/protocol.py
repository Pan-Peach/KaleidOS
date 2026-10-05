"""Validate a complete CoreTest KTAP stream, without a second case inventory."""
import re
from common import RunFailure


def core_test_report(output):
    lines = [line[len("[core-test] "):] for line in output.splitlines()
             if line.startswith("[core-test] ")]
    versions = [i for i, line in enumerate(lines) if line == "KTAP version 1"]
    plans = [i for i, line in enumerate(lines) if re.fullmatch(r"1\.\.[0-9]+", line)]
    if len(versions) != 1 or len(plans) != 1:
        raise RunFailure("missing or duplicate CoreTest KTAP header/plan")
    count = int(lines[plans[0]][3:])
    if count == 0 or versions[0] >= plans[0]:
        raise RunFailure("empty or invalid CoreTest plan")
    results = []
    names = set()
    for index, line in enumerate(lines):
        if line.startswith(("ok ", "not ok ")):
            match = re.fullmatch(r"ok ([0-9]+) - ([A-Za-z0-9_-]+)", line)
            if not match or not versions[0] < index < plans[0]:
                raise RunFailure(f"failed, skipped or malformed CoreTest result: {line}")
            number, name = int(match[1]), match[2]
            if number != len(results) + 1 or name in names:
                raise RunFailure(f"duplicate or out-of-order CoreTest result: {line}")
            results.append(name)
            names.add(name)
    if len(results) != count or "all: PASS" not in lines[plans[0] + 1:]:
        raise RunFailure(f"incomplete CoreTest report: {len(results)}/{count}")
    return results

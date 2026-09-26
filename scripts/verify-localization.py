#!/usr/bin/env python3
"""Check that every application translation key has complete en/fr resources."""

import json
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CATALOG = ROOT / "apps/macos/SailOver/Resources/Localizable.xcstrings"
PLACEHOLDER = re.compile(r"%(?:(\d+)\$)?([-+0 #]*\d*(?:\.\d+)?(?:ll|l)?[@diufgs])")
UI_LITERAL = re.compile(
    r'\b(?:Text|Button|Label|Window|Toggle|Picker|TextField|SecureField|ProgressView|'
    r'help|accessibilityLabel|accessibilityHint|navigationTitle|errorAlert)'
    r'\(\s*(?:verbatim:\s*)?"([^"\n]*)"'
)


def formats(localization):
    if "stringUnit" in localization:
        unit = localization["stringUnit"]
        assert unit["state"] == "translated", "Unfinished translation"
        assert unit["value"], "Empty translation"
        return [unit["value"]]
    plurals = localization["variations"]["plural"]
    assert {"one", "other"} <= plurals.keys(), "Missing plural forms"
    return [value for variant in plurals.values() for value in formats(variant)]


def signature(value):
    matches = PLACEHOLDER.findall(value.replace("%%", ""))
    return sorted((int(position or index), kind) for index, (position, kind) in enumerate(matches, 1))


def main():
    catalog = json.loads(CATALOG.read_text())
    assert catalog["sourceLanguage"] == "en", "English must be the source language"
    entries = catalog["strings"]
    for key, entry in entries.items():
        languages = entry["localizations"]
        assert {"en", "fr"} <= languages.keys(), f"Missing language: {key}"
        variants = [value for language in ("en", "fr") for value in formats(languages[language])]
        assert all(signature(value) == signature(variants[0]) for value in variants), f"Placeholder mismatch: {key}"

    referenced = set()
    for path in (ROOT / "apps/macos/SailOver").rglob("*.swift"):
        if "Generated" in path.parts:
            continue
        source = path.read_text()
        referenced.update(re.findall(r'L10n\.string\("([^"]+)"', source))
        for match in UI_LITERAL.finditer(source):
            assert not any(character.isalpha() for character in match[1]), (
                f"Hardcoded UI text in {path.relative_to(ROOT)}: {match[1]}"
            )
    for path in (ROOT / "crates").rglob("*.rs"):
        referenced.update(re.findall(r'"(backend\.[a-zA-Z0-9_.]+)"', path.read_text()))
    missing = referenced - entries.keys()
    assert not missing, f"Untranslated keys: {sorted(missing)}"
    print(f"Localization verified: {len(entries)} keys, complete English/French text and matching placeholders.")


if __name__ == "__main__":
    main()

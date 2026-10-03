package solder

import (
	"encoding/json"
	"reflect"
	"testing"
)

// The reader agrees with encoding/json on what the editor sends.
func TestParse(t *testing.T) {
	for _, text := range []string{
		`null`, `true`, `false`, `0`, `-12.5e2`, `""`, `"plain"`,
		`"a\"b\\c\/d\n\t\r\b\f"`, `"é é 😀 😀"`,
		`[]`, `[1, "two", [3], {"four": null}]`, `{}`,
		`{"Ok":{"path":"src/a.rs","language":null,"text":"fn main() {\n}\n","selection_start":3,"selection_end":7}}`,
		`{"Err":"The plugin did not declare the permission fs:read"}` + "\n",
		` { "a" : [ true , false ] } `,
	} {
		var want any
		if err := json.Unmarshal([]byte(text), &want); err != nil {
			t.Fatalf("%s: %v", text, err)
		}
		got, err := parse([]byte(text))
		if err != nil || !reflect.DeepEqual(got, want) {
			t.Errorf("%s: got %#v (%v), want %#v", text, got, err, want)
		}
	}
	for _, text := range []string{``, `{`, `[1,`, `"open`, `{"a" 1}`, `tru`, `1 2`, `"\q"`, `{"a":1,}x`} {
		if value, err := parse([]byte(text)); err == nil {
			t.Errorf("%q: read as %#v", text, value)
		}
	}
}

// What quote writes, encoding/json reads back as the same text.
func TestQuote(t *testing.T) {
	for _, text := range []string{"", "plain", "a\"b\\c", "line\nbreak\ttab\r", "\x00\x1f", "é 😀", "</script>"} {
		var back string
		quoted := quote(nil, text)
		if err := json.Unmarshal(quoted, &back); err != nil || back != text {
			t.Errorf("%q: quoted as %s, read back as %q (%v)", text, quoted, back, err)
		}
	}
}

package solder

// A small JSON reader and writer for the lines exchanged with the editor.
// encoding/json reads through reflection, and inside the sandbox's
// interpreter that made reading a 30 000 character file take 25 ms; this
// takes 2.

import (
	"errors"
	"strconv"
	"unicode/utf8"
)

var errJSON = errors.New("not JSON")

// parse reads one JSON value: nil, bool, float64, string, []any or
// map[string]any.
func parse(data []byte) (any, error) {
	p := parser{data: data}
	value, err := p.value()
	if err != nil {
		return nil, err
	}
	p.space()
	if p.at != len(p.data) {
		return nil, errJSON
	}
	return value, nil
}

type parser struct {
	data []byte
	at   int
}

func (p *parser) space() {
	for p.at < len(p.data) {
		switch p.data[p.at] {
		case ' ', '\n', '\t', '\r':
			p.at++
		default:
			return
		}
	}
}

func (p *parser) word(word string, value any) (any, error) {
	if len(p.data)-p.at < len(word) || string(p.data[p.at:p.at+len(word)]) != word {
		return nil, errJSON
	}
	p.at += len(word)
	return value, nil
}

func (p *parser) value() (any, error) {
	p.space()
	if p.at >= len(p.data) {
		return nil, errJSON
	}
	switch c := p.data[p.at]; {
	case c == '"':
		return p.str()
	case c == '{':
		p.at++
		object := map[string]any{}
		for first := true; ; first = false {
			p.space()
			if p.at < len(p.data) && p.data[p.at] == '}' {
				p.at++
				return object, nil
			}
			if !first {
				if p.at >= len(p.data) || p.data[p.at] != ',' {
					return nil, errJSON
				}
				p.at++
				p.space()
			}
			if p.at >= len(p.data) || p.data[p.at] != '"' {
				return nil, errJSON
			}
			key, err := p.str()
			if err != nil {
				return nil, err
			}
			p.space()
			if p.at >= len(p.data) || p.data[p.at] != ':' {
				return nil, errJSON
			}
			p.at++
			value, err := p.value()
			if err != nil {
				return nil, err
			}
			object[key] = value
		}
	case c == '[':
		p.at++
		list := []any{}
		for first := true; ; first = false {
			p.space()
			if p.at < len(p.data) && p.data[p.at] == ']' {
				p.at++
				return list, nil
			}
			if !first {
				if p.at >= len(p.data) || p.data[p.at] != ',' {
					return nil, errJSON
				}
				p.at++
			}
			value, err := p.value()
			if err != nil {
				return nil, err
			}
			list = append(list, value)
		}
	case c == 't':
		return p.word("true", true)
	case c == 'f':
		return p.word("false", false)
	case c == 'n':
		return p.word("null", nil)
	default:
		start := p.at
		for p.at < len(p.data) {
			switch p.data[p.at] {
			case '+', '-', '.', 'e', 'E', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9':
				p.at++
				continue
			}
			break
		}
		number, err := strconv.ParseFloat(string(p.data[start:p.at]), 64)
		if err != nil {
			return nil, errJSON
		}
		return number, nil
	}
}

// str reads the string that starts at the quote under p.at.
func (p *parser) str() (string, error) {
	p.at++
	start := p.at
	// Most strings have no escapes: one pass to the closing quote.
	for p.at < len(p.data) && p.data[p.at] != '"' && p.data[p.at] != '\\' {
		p.at++
	}
	if p.at >= len(p.data) {
		return "", errJSON
	}
	if p.data[p.at] == '"' {
		p.at++
		return string(p.data[start : p.at-1]), nil
	}
	out := make([]byte, 0, len(p.data)-start)
	out = append(out, p.data[start:p.at]...)
	for p.at < len(p.data) {
		c := p.data[p.at]
		p.at++
		switch {
		case c == '"':
			return string(out), nil
		case c != '\\':
			// A run of plain bytes at once.
			from := p.at - 1
			for p.at < len(p.data) && p.data[p.at] != '"' && p.data[p.at] != '\\' {
				p.at++
			}
			out = append(out, p.data[from:p.at]...)
		default:
			if p.at >= len(p.data) {
				return "", errJSON
			}
			escape := p.data[p.at]
			p.at++
			switch escape {
			case 'n':
				out = append(out, '\n')
			case 't':
				out = append(out, '\t')
			case 'r':
				out = append(out, '\r')
			case 'b':
				out = append(out, '\b')
			case 'f':
				out = append(out, '\f')
			case '"', '\\', '/':
				out = append(out, escape)
			case 'u':
				r, ok := p.hex4()
				if !ok {
					return "", errJSON
				}
				// A character above U+FFFF comes as a pair.
				if r >= 0xd800 && r < 0xdc00 && p.at+1 < len(p.data) && p.data[p.at] == '\\' && p.data[p.at+1] == 'u' {
					p.at += 2
					low, ok := p.hex4()
					if !ok {
						return "", errJSON
					}
					r = 0x10000 + (r-0xd800)<<10 + (low - 0xdc00)
				}
				out = utf8.AppendRune(out, r)
			default:
				return "", errJSON
			}
		}
	}
	return "", errJSON
}

func (p *parser) hex4() (rune, bool) {
	if p.at+4 > len(p.data) {
		return 0, false
	}
	n, err := strconv.ParseUint(string(p.data[p.at:p.at+4]), 16, 32)
	p.at += 4
	return rune(n), err == nil
}

// quote appends text to out as a JSON string.
func quote(out []byte, text string) []byte {
	out = append(out, '"')
	start := 0
	for i := 0; i < len(text); i++ {
		c := text[i]
		if c >= 0x20 && c != '"' && c != '\\' {
			continue
		}
		out = append(out, text[start:i]...)
		switch c {
		case '"', '\\':
			out = append(out, '\\', c)
		case '\n':
			out = append(out, '\\', 'n')
		case '\t':
			out = append(out, '\\', 't')
		case '\r':
			out = append(out, '\\', 'r')
		default:
			const hex = "0123456789abcdef"
			out = append(out, '\\', 'u', '0', '0', hex[c>>4], hex[c&15])
		}
		start = i + 1
	}
	out = append(out, text[start:]...)
	return append(out, '"')
}

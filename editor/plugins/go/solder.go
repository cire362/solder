// Package solder is what a Solder plugin written in Go uses.
//
// A plugin is a program built for WASI,
//
//	GOOS=wasip1 GOARCH=wasm go build -o plugin.wasm .
//
// next to a plugin.json that names it and lists the permissions it needs.
// It registers handlers and calls Run:
//
//	func main() {
//		solder.On("open", func(solder.Event) { solder.Status("hello") })
//		solder.Run()
//	}
//
// The editor runs the program in a sandbox with no files and no network.
// It hands it events as lines on its input and answers its requests, if the
// manifest declared the permission, on the same lines. Anything the program
// prints goes to the plugin's log.
package solder

import (
	"bufio"
	"errors"
	"fmt"
	"os"
	"strconv"
)

// Event is something that happened in the editor.
type Event struct {
	// "activate", "command", "open", "save" or "change".
	Event string
	// The command, for "command".
	ID string
	// The file, from the project's root, for "open", "save" and "change".
	Path string
	// The file's language, for "open"; nil when it has none.
	Language *string
}

// File is the file in front and where its selection is, in bytes.
type File struct {
	// Nil for a file not saved yet.
	Path           *string
	Language       *string
	Text           string
	SelectionStart int
	SelectionEnd   int
}

// Request is an HTTP request; the method is GET when left empty.
type Request struct {
	Method  string
	URL     string
	Headers map[string]string
	Body    string
}

// Response is the answer to a Request. A redirect is not followed.
type Response struct {
	Status  int
	Headers [][2]string
	Body    string
}

var (
	input    = bufio.NewReaderSize(os.Stdin, 1<<16)
	output   = bufio.NewWriter(os.Stdout)
	handlers = map[string][]func(Event){}
	commands = map[string]func(){}
)

// On runs handler for an event the manifest lists under "events".
func On(event string, handler func(Event)) {
	handlers[event] = append(handlers[event], handler)
}

// Command runs handler for a command the manifest lists under "commands".
func Command(id string, handler func()) {
	commands[id] = handler
}

// field is one member of a request.
type field struct {
	name  string
	value any
}

// ask sends one request and returns the editor's answer: the value, or the
// reason it was refused.
func ask(call string, fields ...field) (any, error) {
	// A request is a line that starts with \x01.
	line := append([]byte{1}, `{"call":`...)
	line = quote(line, call)
	for _, f := range fields {
		line = append(line, ',')
		line = quote(line, f.name)
		line = append(line, ':')
		switch value := f.value.(type) {
		case string:
			line = quote(line, value)
		case int:
			line = strconv.AppendInt(line, int64(value), 10)
		case [][2]string:
			line = append(line, '[')
			for i, pair := range value {
				if i > 0 {
					line = append(line, ',')
				}
				line = append(line, '[')
				line = quote(line, pair[0])
				line = append(line, ',')
				line = quote(line, pair[1])
				line = append(line, ']')
			}
			line = append(line, ']')
		}
	}
	line = append(line, '}', '\n')
	output.Write(line)
	output.Flush()
	answer, err := input.ReadBytes('\n')
	if err != nil {
		return nil, err
	}
	value, err := parse(answer)
	if err != nil {
		return nil, err
	}
	reply, _ := value.(map[string]any)
	if refused, ok := reply["Err"].(string); ok {
		return nil, errors.New(refused)
	}
	return reply["Ok"], nil
}

// text is a member that may be absent.
func text(object map[string]any, name string) *string {
	if value, ok := object[name].(string); ok {
		return &value
	}
	return nil
}

func number(object map[string]any, name string) int {
	value, _ := object[name].(float64)
	return int(value)
}

// Status shows text in the status bar; empty text removes it. Needs
// "statusBar".
func Status(text string) {
	ask("status", field{"text", text})
}

// Log adds a line to the plugin's log, shown in the Plugins window.
func Log(text string) {
	ask("log", field{"text", text})
}

// Editor returns the file in front. Needs "editor:read".
func Editor() (File, error) {
	value, err := ask("editor_text")
	object, _ := value.(map[string]any)
	if err != nil || object == nil {
		return File{}, err
	}
	content, _ := object["text"].(string)
	return File{
		Path:           text(object, "path"),
		Language:       text(object, "language"),
		Text:           content,
		SelectionStart: number(object, "selection_start"),
		SelectionEnd:   number(object, "selection_end"),
	}, nil
}

// Edit replaces bytes start..end of the file in front, which must still be
// path. Needs "editor:write".
func Edit(path string, start, end int, text string) error {
	_, err := ask("edit", field{"path", path}, field{"start", start}, field{"end", end}, field{"text", text})
	return err
}

// ReadFile returns a file of the project, by its path from the root. Needs
// "fs:read".
func ReadFile(path string) (string, error) {
	value, err := ask("read_file", field{"path", path})
	content, _ := value.(string)
	return content, err
}

// HTTP sends a request. Needs "http:<host>" for the URL's host.
func HTTP(request Request) (Response, error) {
	method := request.Method
	if method == "" {
		method = "GET"
	}
	headers := [][2]string{}
	for name, value := range request.Headers {
		headers = append(headers, [2]string{name, value})
	}
	fields := []field{{"method", method}, {"url", request.URL}, {"headers", headers}}
	if request.Body != "" {
		fields = append(fields, field{"body", request.Body})
	}
	value, err := ask("http", fields...)
	object, _ := value.(map[string]any)
	if err != nil || object == nil {
		return Response{}, err
	}
	response := Response{Status: number(object, "status")}
	response.Body, _ = object["body"].(string)
	pairs, _ := object["headers"].([]any)
	for _, pair := range pairs {
		if pair, ok := pair.([]any); ok && len(pair) == 2 {
			name, _ := pair[0].(string)
			value, _ := pair[1].(string)
			response.Headers = append(response.Headers, [2]string{name, value})
		}
	}
	return response, nil
}

// handle runs one event's handlers. A panic fails that event only.
func handle(event Event) {
	defer func() {
		if thrown := recover(); thrown != nil {
			// A line that starts with \x02 says the event failed.
			output.WriteByte(2)
			fmt.Fprintf(output, "panic: %v\n", thrown)
			output.Flush()
		}
	}()
	if event.Event == "command" {
		if handler := commands[event.ID]; handler != nil {
			handler()
		}
		return
	}
	for _, handler := range handlers[event.Event] {
		handler(event)
	}
}

// Run hands events to the handlers until the editor stops the plugin. Call
// it last in main.
func Run() {
	for {
		line, err := input.ReadBytes('\n')
		if err != nil {
			return
		}
		value, err := parse(line)
		object, _ := value.(map[string]any)
		if err != nil || object == nil {
			continue
		}
		event := Event{Language: text(object, "language")}
		event.Event, _ = object["event"].(string)
		event.ID, _ = object["id"].(string)
		event.Path, _ = object["path"].(string)
		handle(event)
	}
}

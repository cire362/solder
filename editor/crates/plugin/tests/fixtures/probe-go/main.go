// The program the host's tests run: the Go twin of `probe`. A command's id
// says what to do, with an argument after a colon; the outcome goes to the
// status bar.
package main

import (
	"fmt"
	"os"
	"strings"
	"time"

	"solder"
)

var events = 0

// Work the editor can time.
func spin(rounds uint64) uint64 {
	var x uint64
	for i := uint64(0); i < rounds; i++ {
		x = x*6364136223846793005 + i
	}
	return x
}

var sink uint64

func show(text string, err error) {
	if err != nil {
		text = "refused: " + err.Error()
	}
	solder.Status(text)
}

func run(id string) {
	events++
	name, arg, _ := strings.Cut(id, ":")
	switch name {
	case "count":
		solder.Status(fmt.Sprintf("%d events", events))
	case "editor":
		file, err := solder.Editor()
		path := ""
		if file.Path != nil {
			path = *file.Path
		}
		show(fmt.Sprintf("%s %d..%d %d", path, file.SelectionStart, file.SelectionEnd, len(file.Text)), err)
	case "shout":
		file, err := solder.Editor()
		if err == nil {
			show("edited", solder.Edit(*file.Path, 0, len(file.Text), strings.ToUpper(file.Text)))
		}
	case "read":
		show(solder.ReadFile(arg))
	case "get":
		response, err := solder.HTTP(solder.Request{URL: arg, Headers: map[string]string{"x-probe": "1"}})
		show(fmt.Sprintf("%d %s", response.Status, response.Body), err)
	case "forever":
		for {
			sink += spin(1000000)
		}
	case "panic":
		panic("probe asked to panic")
	case "file":
		// There are no files here: the program loads, and this fails.
		_, err := os.ReadFile(arg)
		show("read it", err)
	case "print":
		fmt.Println(arg)
		solder.Status("printed")
	case "nap":
		started := time.Now()
		time.Sleep(60 * time.Millisecond)
		solder.Status(fmt.Sprintf("slept %v", time.Since(started) >= 60*time.Millisecond))
	case "alloc":
		var size int
		fmt.Sscan(arg, &size)
		solder.Status(fmt.Sprintf("allocated %d", len(make([]byte, size))))
	}
}

func main() {
	solder.On("activate", func(solder.Event) { events++; solder.Status("active") })
	solder.On("open", func(e solder.Event) { events++; solder.Status("open " + e.Path) })
	solder.On("save", func(e solder.Event) { events++; solder.Status("save " + e.Path) })
	// Typing: enough work to go over a small budget.
	solder.On("change", func(e solder.Event) {
		events++
		sink += spin(200000)
		solder.Status("change " + e.Path)
	})
	for _, id := range []string{
		"count", "editor", "shout", "read:notes.txt", "read:missing.txt",
		"get:http://127.0.0.1:8080/x", "get:http://example.com/x",
		"forever", "panic", "print:hello from go", "nap", "alloc:1000", "alloc:200000000",
		"file:/etc/passwd",
	} {
		id := id
		solder.Command(id, func() { run(id) })
	}
	solder.Run()
}

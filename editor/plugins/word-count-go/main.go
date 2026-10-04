// The example plugin in Go: the word count of the file in front, in the
// status bar, and a command that writes it at the cursor.
//
//	GOOS=wasip1 GOARCH=wasm go build -ldflags="-s -w" -o plugin.wasm .
//
// then copy plugin.json and plugin.wasm into a folder under Solder's plugins
// folder.
package main

import (
	"fmt"
	"strings"

	"solder"
)

func label(text string) string {
	if count := len(strings.Fields(text)); count != 1 {
		return fmt.Sprintf("%d words", count)
	}
	return "1 word"
}

func show(solder.Event) {
	if file, err := solder.Editor(); err == nil {
		solder.Status(label(file.Text))
	}
}

func main() {
	solder.On("open", show)
	solder.On("change", show)
	solder.On("save", show)
	solder.Command("insert", func() {
		file, err := solder.Editor()
		if err != nil || file.Path == nil {
			return
		}
		solder.Edit(*file.Path, file.SelectionStart, file.SelectionEnd, label(file.Text))
	})
	solder.Run()
}

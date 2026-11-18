package main

import "net/http"

func handler(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet { http.Error(w, "nope", 405); return }
	w.Write([]byte(`{"ok": true}`)) // raw string
}

package inject

import (
	"bufio"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/caddyserver/caddy/v2/modules/caddyhttp"
)

// fakeHandler writes a response with the given status, content type, and body.
func fakeHandler(status int, contentType string, body string) caddyhttp.Handler {
	return caddyhttp.HandlerFunc(func(w http.ResponseWriter, r *http.Request) error {
		if contentType != "" {
			w.Header().Set("Content-Type", contentType)
		}
		if body != "" {
			w.Header().Set("Content-Length", fmt.Sprintf("%d", len(body)))
		}
		w.WriteHeader(status)
		_, err := w.Write([]byte(body))
		return err
	})
}

// fakeMultiChunkHandler writes the body in multiple chunks.
func fakeMultiChunkHandler(contentType string, chunks []string) caddyhttp.Handler {
	return caddyhttp.HandlerFunc(func(w http.ResponseWriter, r *http.Request) error {
		w.Header().Set("Content-Type", contentType)
		w.WriteHeader(http.StatusOK)
		for _, chunk := range chunks {
			if _, err := w.Write([]byte(chunk)); err != nil {
				return err
			}
		}
		return nil
	})
}

func TestHTMLWithDoctype(t *testing.T) {
	vi := VeldInject{Prefix: "<script>boot()</script>"}
	rec := httptest.NewRecorder()
	req := httptest.NewRequest("GET", "/", nil)

	err := vi.ServeHTTP(rec, req, fakeHandler(200, "text/html; charset=utf-8",
		"<!DOCTYPE html><html><body>Hello</body></html>"))
	if err != nil {
		t.Fatal(err)
	}

	expected := "<!DOCTYPE html><script>boot()</script><html><body>Hello</body></html>"
	if got := rec.Body.String(); got != expected {
		t.Errorf("body = %q, want %q", got, expected)
	}
	if cl := rec.Header().Get("Content-Length"); cl != "" {
		t.Errorf("Content-Length should be removed, got %q", cl)
	}
}

func TestHTMLWithoutDoctype(t *testing.T) {
	vi := VeldInject{Prefix: "<script>boot()</script>"}
	rec := httptest.NewRecorder()
	req := httptest.NewRequest("GET", "/", nil)

	err := vi.ServeHTTP(rec, req, fakeHandler(200, "text/html",
		"<html><body>Hello</body></html>"))
	if err != nil {
		t.Fatal(err)
	}

	// No DOCTYPE — prefix goes before everything.
	expected := "<script>boot()</script><html><body>Hello</body></html>"
	if got := rec.Body.String(); got != expected {
		t.Errorf("body = %q, want %q", got, expected)
	}
}

func TestHTMLCharsetVariants(t *testing.T) {
	cases := []string{
		"text/html",
		"text/html; charset=utf-8",
		"text/html;charset=ISO-8859-1",
	}
	for _, ct := range cases {
		t.Run(ct, func(t *testing.T) {
			vi := VeldInject{Prefix: "PREFIX"}
			rec := httptest.NewRecorder()
			req := httptest.NewRequest("GET", "/", nil)

			err := vi.ServeHTTP(rec, req, fakeHandler(200, ct, "<!DOCTYPE html>BODY"))
			if err != nil {
				t.Fatal(err)
			}
			if got := rec.Body.String(); got != "<!DOCTYPE html>PREFIXBODY" {
				t.Errorf("body = %q, want %q", got, "<!DOCTYPE html>PREFIXBODY")
			}
		})
	}
}

func TestNonHTMLPassthrough(t *testing.T) {
	vi := VeldInject{Prefix: "SHOULD NOT APPEAR"}
	rec := httptest.NewRecorder()
	req := httptest.NewRequest("GET", "/api/data", nil)

	body := `{"key":"value"}`
	err := vi.ServeHTTP(rec, req, fakeHandler(200, "application/json", body))
	if err != nil {
		t.Fatal(err)
	}

	if got := rec.Body.String(); got != body {
		t.Errorf("body = %q, want %q", got, body)
	}
	if cl := rec.Header().Get("Content-Length"); cl != fmt.Sprintf("%d", len(body)) {
		t.Errorf("Content-Length = %q, want %q", cl, fmt.Sprintf("%d", len(body)))
	}
}

func TestEmptyPrefix(t *testing.T) {
	vi := VeldInject{Prefix: ""}
	rec := httptest.NewRecorder()
	req := httptest.NewRequest("GET", "/", nil)

	err := vi.ServeHTTP(rec, req, fakeHandler(200, "text/html", "<html>OK</html>"))
	if err != nil {
		t.Fatal(err)
	}

	if got := rec.Body.String(); got != "<html>OK</html>" {
		t.Errorf("body = %q, want %q", got, "<html>OK</html>")
	}
}

func TestPrefixOnlyOnFirstWrite(t *testing.T) {
	vi := VeldInject{Prefix: "PRE"}
	rec := httptest.NewRecorder()
	req := httptest.NewRequest("GET", "/", nil)

	chunks := []string{"<!DOCTYPE html>", "<html>", "<body>streaming</body></html>"}
	err := vi.ServeHTTP(rec, req, fakeMultiChunkHandler("text/html", chunks))
	if err != nil {
		t.Fatal(err)
	}

	// Prefix goes after DOCTYPE in first chunk, not repeated in subsequent chunks.
	expected := "<!DOCTYPE html>PRE<html><body>streaming</body></html>"
	if got := rec.Body.String(); got != expected {
		t.Errorf("body = %q, want %q", got, expected)
	}
}

func Test304NotModified(t *testing.T) {
	vi := VeldInject{Prefix: "SHOULD NOT APPEAR"}
	rec := httptest.NewRecorder()
	req := httptest.NewRequest("GET", "/", nil)

	err := vi.ServeHTTP(rec, req, fakeHandler(304, "text/html", ""))
	if err != nil {
		t.Fatal(err)
	}

	if got := rec.Body.String(); got != "" {
		t.Errorf("body should be empty for 304, got %q", got)
	}
}

func Test204NoContent(t *testing.T) {
	vi := VeldInject{Prefix: "SHOULD NOT APPEAR"}
	rec := httptest.NewRecorder()
	req := httptest.NewRequest("GET", "/", nil)

	err := vi.ServeHTTP(rec, req, fakeHandler(204, "text/html", ""))
	if err != nil {
		t.Fatal(err)
	}

	if got := rec.Body.String(); got != "" {
		t.Errorf("body should be empty for 204, got %q", got)
	}
}

func TestNoContentType(t *testing.T) {
	vi := VeldInject{Prefix: "SHOULD NOT APPEAR"}
	rec := httptest.NewRecorder()
	req := httptest.NewRequest("GET", "/", nil)

	err := vi.ServeHTTP(rec, req, fakeHandler(200, "", "raw bytes"))
	if err != nil {
		t.Fatal(err)
	}

	if got := rec.Body.String(); got != "raw bytes" {
		t.Errorf("body = %q, want %q", got, "raw bytes")
	}
}

func TestErrorPageInjected(t *testing.T) {
	cases := []int{400, 404, 500, 502, 503}
	for _, code := range cases {
		t.Run(fmt.Sprintf("status_%d", code), func(t *testing.T) {
			vi := VeldInject{Prefix: "PRE"}
			rec := httptest.NewRecorder()
			req := httptest.NewRequest("GET", "/", nil)

			err := vi.ServeHTTP(rec, req, fakeHandler(code, "text/html", "<!DOCTYPE html><html>Error</html>"))
			if err != nil {
				t.Fatal(err)
			}

			expected := "<!DOCTYPE html>PRE<html>Error</html>"
			if got := rec.Body.String(); got != expected {
				t.Errorf("status %d: body = %q, want %q", code, got, expected)
			}
		})
	}
}

func Test1xxNoInjection(t *testing.T) {
	if shouldInject(100, "text/html") {
		t.Error("shouldInject(100, text/html) should be false")
	}
	if shouldInject(101, "text/html") {
		t.Error("shouldInject(101, text/html) should be false")
	}
}

func TestCSSPassthrough(t *testing.T) {
	vi := VeldInject{Prefix: "NOPE"}
	rec := httptest.NewRecorder()
	req := httptest.NewRequest("GET", "/style.css", nil)

	err := vi.ServeHTTP(rec, req, fakeHandler(200, "text/css", "body{color:red}"))
	if err != nil {
		t.Fatal(err)
	}

	if got := rec.Body.String(); got != "body{color:red}" {
		t.Errorf("CSS should pass through, got %q", got)
	}
}

func TestEventStreamPassthrough(t *testing.T) {
	vi := VeldInject{Prefix: "NOPE"}
	rec := httptest.NewRecorder()
	req := httptest.NewRequest("GET", "/events", nil)

	err := vi.ServeHTTP(rec, req, fakeHandler(200, "text/event-stream", "data: hello\n\n"))
	if err != nil {
		t.Fatal(err)
	}

	if got := rec.Body.String(); got != "data: hello\n\n" {
		t.Errorf("SSE should pass through, got %q", got)
	}
}

// --- Flusher delegation ---

type flushRecorder struct {
	*httptest.ResponseRecorder
	flushed int
}

func (f *flushRecorder) Flush() {
	f.flushed++
	f.ResponseRecorder.Flush()
}

func TestFlushDelegated(t *testing.T) {
	vi := VeldInject{Prefix: "PRE"}
	rec := &flushRecorder{ResponseRecorder: httptest.NewRecorder()}
	req := httptest.NewRequest("GET", "/", nil)

	handler := caddyhttp.HandlerFunc(func(w http.ResponseWriter, r *http.Request) error {
		w.Header().Set("Content-Type", "text/html")
		w.WriteHeader(200)
		w.Write([]byte("<html>"))
		if f, ok := w.(http.Flusher); ok {
			f.Flush()
		}
		w.Write([]byte("</html>"))
		return nil
	})

	err := vi.ServeHTTP(rec, req, handler)
	if err != nil {
		t.Fatal(err)
	}

	if rec.flushed == 0 {
		t.Error("Flush was not delegated to the underlying writer")
	}

	// No DOCTYPE — prefix before everything.
	expected := "PRE<html></html>"
	if got := rec.Body.String(); got != expected {
		t.Errorf("body = %q, want %q", got, expected)
	}
}

// --- Hijacker delegation ---

type hijackRecorder struct {
	*httptest.ResponseRecorder
	hijacked bool
}

func (h *hijackRecorder) Hijack() (net.Conn, *bufio.ReadWriter, error) {
	h.hijacked = true
	server, _ := net.Pipe()
	return server, bufio.NewReadWriter(bufio.NewReader(server), bufio.NewWriter(server)), nil
}

func TestHijackDelegated(t *testing.T) {
	vi := VeldInject{Prefix: "PRE"}
	rec := &hijackRecorder{ResponseRecorder: httptest.NewRecorder()}
	req := httptest.NewRequest("GET", "/ws", nil)

	handler := caddyhttp.HandlerFunc(func(w http.ResponseWriter, r *http.Request) error {
		if hj, ok := w.(http.Hijacker); ok {
			conn, _, err := hj.Hijack()
			if err != nil {
				return err
			}
			conn.Close()
		}
		return nil
	})

	err := vi.ServeHTTP(rec, req, handler)
	if err != nil {
		t.Fatal(err)
	}

	if !rec.hijacked {
		t.Error("Hijack was not delegated to the underlying writer")
	}
}

func TestHijackFailsWithoutSupport(t *testing.T) {
	vi := VeldInject{Prefix: "PRE"}
	rec := httptest.NewRecorder()
	req := httptest.NewRequest("GET", "/ws", nil)

	var hijackErr error
	handler := caddyhttp.HandlerFunc(func(w http.ResponseWriter, r *http.Request) error {
		if hj, ok := w.(http.Hijacker); ok {
			_, _, hijackErr = hj.Hijack()
		}
		return nil
	})

	err := vi.ServeHTTP(rec, req, handler)
	if err != nil {
		t.Fatal(err)
	}

	if hijackErr == nil {
		t.Error("Hijack should fail when underlying writer doesn't support it")
	}
	if !strings.Contains(hijackErr.Error(), "does not implement") {
		t.Errorf("unexpected error: %v", hijackErr)
	}
}

// --- Unwrap ---

func TestUnwrap(t *testing.T) {
	inner := httptest.NewRecorder()
	ri := &responseInterceptor{
		ResponseWriter: inner,
		prefix:         []byte("test"),
	}
	if ri.Unwrap() != inner {
		t.Error("Unwrap should return the inner ResponseWriter")
	}
}

// --- shouldInject unit tests ---

func TestShouldInject(t *testing.T) {
	cases := []struct {
		name        string
		status      int
		contentType string
		want        bool
	}{
		{"html 200", 200, "text/html", true},
		{"html charset 200", 200, "text/html; charset=utf-8", true},
		{"json 200", 200, "application/json", false},
		{"css 200", 200, "text/css", false},
		{"empty ct", 200, "", false},
		{"html 404", 404, "text/html", true},
		{"html 500", 500, "text/html", true},
		{"html 204", 204, "text/html", false},
		{"html 304", 304, "text/html", false},
		{"html 100", 100, "text/html", false},
		{"html 101", 101, "text/html", false},
		{"event-stream", 200, "text/event-stream", false},
		{"text/plain", 200, "text/plain", false},
		{"html 201", 201, "text/html", true},
		{"html 301", 301, "text/html", true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := shouldInject(tc.status, tc.contentType)
			if got != tc.want {
				t.Errorf("shouldInject(%d, %q) = %v, want %v", tc.status, tc.contentType, got, tc.want)
			}
		})
	}
}

// --- doctypeEnd unit tests ---

func TestDoctypeEnd(t *testing.T) {
	cases := []struct {
		name string
		body string
		want int
	}{
		{"standard", "<!DOCTYPE html><html>", 15},
		{"lowercase", "<!doctype html><html>", 15},
		{"mixed case", "<!DoCtYpE HtMl><html>", 15},
		{"with system id", "<!DOCTYPE html SYSTEM \"about:legacy-compat\"><html>", 44},
		{"leading whitespace", "  <!DOCTYPE html><html>", 17},
		{"leading newline", "\n<!DOCTYPE html><html>", 16},
		{"no doctype", "<html><head>", -1},
		{"empty", "", -1},
		{"just html", "<html>", -1},
		{"partial doctype", "<!doctyp", -1},
		{"doctype not at start", "<html><!DOCTYPE html>", -1},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := doctypeEnd([]byte(tc.body))
			if got != tc.want {
				t.Errorf("doctypeEnd(%q) = %d, want %d", tc.body, got, tc.want)
			}
		})
	}
}

// --- CaddyModule registration ---

func TestCaddyModuleInfo(t *testing.T) {
	vi := VeldInject{}
	info := vi.CaddyModule()
	if info.ID != "http.handlers.veld_inject" {
		t.Errorf("module ID = %q, want %q", info.ID, "http.handlers.veld_inject")
	}
	if info.New == nil {
		t.Error("New function should not be nil")
	}
	mod := info.New()
	if _, ok := mod.(*VeldInject); !ok {
		t.Error("New() should return *VeldInject")
	}
}

// --- Large prefix / body ---

func TestLargePrefixAndBody(t *testing.T) {
	prefix := strings.Repeat("<script>x</script>", 100)
	body := "<!DOCTYPE html>" + strings.Repeat("<p>paragraph</p>", 1000)

	vi := VeldInject{Prefix: prefix}
	rec := httptest.NewRecorder()
	req := httptest.NewRequest("GET", "/", nil)

	err := vi.ServeHTTP(rec, req, fakeHandler(200, "text/html", body))
	if err != nil {
		t.Fatal(err)
	}

	got := rec.Body.String()
	expectedStart := "<!DOCTYPE html>" + prefix
	if !strings.HasPrefix(got, expectedStart) {
		t.Error("response should start with DOCTYPE then prefix")
	}
	if len(got) != len(prefix)+len(body) {
		t.Errorf("total length = %d, want %d", len(got), len(prefix)+len(body))
	}
}

// --- Interface compliance at runtime ---

func TestResponseInterceptorImplementsInterfaces(t *testing.T) {
	ri := &responseInterceptor{
		ResponseWriter: httptest.NewRecorder(),
	}
	var _ http.ResponseWriter = ri
	var _ http.Flusher = ri
	var _ http.Hijacker = ri
	var _ io.Writer = ri
}

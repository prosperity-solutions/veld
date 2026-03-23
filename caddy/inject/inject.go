// Package inject provides a Caddy HTTP handler that injects a configurable
// prefix into HTML responses without buffering. This enables streaming SSR
// while still injecting bootstrap scripts (e.g. feedback overlay, client-log
// collector).
//
// The handler prepends the prefix to the first body chunk. If the chunk
// starts with a DOCTYPE declaration, the prefix is inserted immediately
// after it to avoid triggering quirks mode. It properly delegates Flusher
// (SSE), Hijacker (WebSocket), and Unwrap so that all HTTP protocols work
// transparently.
package inject

import (
	"bufio"
	"bytes"
	"fmt"
	"net"
	"net/http"
	"strings"

	"github.com/caddyserver/caddy/v2"
	"github.com/caddyserver/caddy/v2/caddyconfig/caddyfile"
	"github.com/caddyserver/caddy/v2/modules/caddyhttp"
)

func init() {
	caddy.RegisterModule(VeldInject{})
}

// VeldInject is a Caddy HTTP handler that injects a prefix string into
// text/html responses. The prefix is prepended to the first body chunk
// (after any DOCTYPE declaration) with zero buffering.
type VeldInject struct {
	// Prefix is the HTML content to prepend (e.g. a <script> tag).
	Prefix string `json:"prefix,omitempty"`
}

// CaddyModule returns the Caddy module information.
func (VeldInject) CaddyModule() caddy.ModuleInfo {
	return caddy.ModuleInfo{
		ID:  "http.handlers.veld_inject",
		New: func() caddy.Module { return new(VeldInject) },
	}
}

// ServeHTTP implements caddyhttp.MiddlewareHandler.
func (vi VeldInject) ServeHTTP(w http.ResponseWriter, r *http.Request, next caddyhttp.Handler) error {
	// Only wrap requests that might return HTML. All others pass through
	// untouched — no wrapper, no Accept-Encoding override, no interference
	// with compression, WebSocket, SSE, or any other protocol.
	if !mightAcceptHTML(r) || isUpgradeRequest(r) || isSSERequest(r) {
		return next.ServeHTTP(w, r)
	}
	// Force uncompressed HTML so we can prepend the bootstrap script.
	r.Header.Set("Accept-Encoding", "identity")
	ri := &responseInterceptor{
		ResponseWriter: w,
		prefix:         []byte(vi.Prefix),
	}
	return next.ServeHTTP(ri, r)
}

// isUpgradeRequest returns true if the request is a WebSocket (or other protocol) upgrade.
func isUpgradeRequest(r *http.Request) bool {
	return strings.EqualFold(r.Header.Get("Connection"), "upgrade") ||
		strings.EqualFold(r.Header.Get("Upgrade"), "websocket")
}

// isSSERequest returns true if the client is requesting Server-Sent Events.
func isSSERequest(r *http.Request) bool {
	return strings.Contains(r.Header.Get("Accept"), "text/event-stream")
}

// mightAcceptHTML returns true if the request's Accept header includes text/html
// or is empty/wildcard (browser navigation). API calls typically send
// Accept: application/json which doesn't match.
func mightAcceptHTML(r *http.Request) bool {
	accept := r.Header.Get("Accept")
	if accept == "" || accept == "*/*" {
		return true
	}
	return strings.Contains(accept, "text/html")
}

// UnmarshalCaddyfile implements caddyfile.Unmarshaler (optional Caddyfile support).
func (vi *VeldInject) UnmarshalCaddyfile(d *caddyfile.Dispenser) error {
	d.Next() // consume directive name
	if d.NextArg() {
		vi.Prefix = d.Val()
	}
	return nil
}

// responseInterceptor wraps an http.ResponseWriter to prepend a prefix to
// text/html response bodies. It delegates Flusher, Hijacker, and Unwrap
// to the underlying writer so WebSocket, SSE, and HTTP/2 work transparently.
type responseInterceptor struct {
	http.ResponseWriter
	prefix    []byte
	matched   bool // Content-Type is text/html
	wroteOnce bool // prefix has been written
}

// WriteHeader inspects the Content-Type and status code to decide whether
// to inject the prefix. For matched responses, Content-Length is removed
// since the response length changes (HTTP will use chunked transfer).
func (ri *responseInterceptor) WriteHeader(code int) {
	if len(ri.prefix) > 0 && shouldInject(code, ri.Header().Get("Content-Type")) {
		ri.matched = true
		ri.Header().Del("Content-Length")
	}
	ri.ResponseWriter.WriteHeader(code)
}

// Write prepends the prefix to the first body chunk for matched responses.
// If the chunk begins with a DOCTYPE declaration, the prefix is inserted
// immediately after it to avoid triggering quirks mode.
func (ri *responseInterceptor) Write(b []byte) (int, error) {
	if ri.matched && !ri.wroteOnce {
		ri.wroteOnce = true

		// If the first chunk starts with <!DOCTYPE, skip past it.
		if pos := doctypeEnd(b); pos > 0 {
			if _, err := ri.ResponseWriter.Write(b[:pos]); err != nil {
				return 0, err
			}
			if _, err := ri.ResponseWriter.Write(ri.prefix); err != nil {
				return 0, err
			}
			_, err := ri.ResponseWriter.Write(b[pos:])
			return len(b), err
		}

		// No DOCTYPE at start — prepend before everything.
		if _, err := ri.ResponseWriter.Write(ri.prefix); err != nil {
			return 0, err
		}
	}
	return ri.ResponseWriter.Write(b)
}

// Flush delegates to the underlying writer if it implements http.Flusher.
// This is critical for Server-Sent Events (SSE) streaming.
func (ri *responseInterceptor) Flush() {
	if f, ok := ri.ResponseWriter.(http.Flusher); ok {
		f.Flush()
	}
}

// Hijack delegates to the underlying writer if it implements http.Hijacker.
// This is critical for WebSocket upgrade requests.
func (ri *responseInterceptor) Hijack() (net.Conn, *bufio.ReadWriter, error) {
	if hj, ok := ri.ResponseWriter.(http.Hijacker); ok {
		return hj.Hijack()
	}
	return nil, nil, fmt.Errorf("upstream ResponseWriter does not implement http.Hijacker")
}

// Unwrap returns the underlying ResponseWriter. Caddy's infrastructure uses
// this to find the original writer for interface checks.
func (ri *responseInterceptor) Unwrap() http.ResponseWriter {
	return ri.ResponseWriter
}

// doctypeEnd returns the byte offset immediately after the closing '>' of a
// leading DOCTYPE declaration, or -1 if the chunk does not start with one.
// The check is case-insensitive and skips leading whitespace.
func doctypeEnd(b []byte) int {
	trimmed := bytes.TrimLeft(b, " \t\r\n")
	skip := len(b) - len(trimmed)
	if len(trimmed) < 9 { // len("<!doctype") == 9
		return -1
	}
	if !strings.EqualFold(string(trimmed[:9]), "<!doctype") {
		return -1
	}
	// Find the closing '>'.
	if end := bytes.IndexByte(trimmed, '>'); end >= 0 {
		return skip + end + 1
	}
	return -1
}

// shouldInject returns true if the response should receive the prefix.
// It checks that the Content-Type starts with "text/html" and the status
// code indicates a response with a body (not 1xx, 204, or 304).
func shouldInject(statusCode int, contentType string) bool {
	if contentType == "" {
		return false
	}
	if !strings.HasPrefix(contentType, "text/html") {
		return false
	}
	// No body expected for these status codes.
	if statusCode == http.StatusNoContent || statusCode == http.StatusNotModified {
		return false
	}
	if statusCode >= 100 && statusCode < 200 {
		return false
	}
	return true
}

// Interface guards — ensure compile-time compliance.
var (
	_ caddyhttp.MiddlewareHandler = (*VeldInject)(nil)
	_ caddyfile.Unmarshaler       = (*VeldInject)(nil)
	_ http.Flusher                = (*responseInterceptor)(nil)
	_ http.Hijacker               = (*responseInterceptor)(nil)
)

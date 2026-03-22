// Package inject provides a Caddy HTTP handler that injects a configurable
// prefix into HTML responses without buffering. This enables streaming SSR
// while still injecting bootstrap scripts (e.g. feedback overlay, client-log
// collector).
//
// Unlike replace-response, this handler never buffers the response body — it
// injects the prefix after <!DOCTYPE html> (or <head>) in the first body
// chunk and then streams the rest. Injecting after the doctype avoids
// triggering quirks mode, which breaks React hydration and other frameworks.
// It properly delegates Flusher (SSE), Hijacker (WebSocket), and Unwrap
// so that all HTTP protocols work transparently.
package inject

import (
	"bufio"
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
// text/html responses after <!DOCTYPE html> (or <head>). The injection
// happens on the first body chunk with zero buffering.
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
	ri := &responseInterceptor{
		ResponseWriter: w,
		prefix:         []byte(vi.Prefix),
	}
	return next.ServeHTTP(ri, r)
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

// Write injects the prefix into the first body chunk for matched responses.
// To avoid quirks mode, the prefix is inserted AFTER <!DOCTYPE html> (or
// after <head...>) rather than before it. If neither marker is found in the
// first chunk, the prefix is appended after the chunk (safe for streaming —
// subsequent chunks won't be inspected).
func (ri *responseInterceptor) Write(b []byte) (int, error) {
	if ri.matched && !ri.wroteOnce {
		ri.wroteOnce = true
		if pos := findInjectionPoint(b); pos >= 0 {
			// Write everything up to the injection point, then the prefix,
			// then the rest. Report len(b) back to the caller since that's
			// how many of *their* bytes were consumed.
			if _, err := ri.ResponseWriter.Write(b[:pos]); err != nil {
				return 0, err
			}
			if _, err := ri.ResponseWriter.Write(ri.prefix); err != nil {
				return 0, err
			}
			_, err := ri.ResponseWriter.Write(b[pos:])
			return len(b), err
		}
		// No marker found — write the chunk first, then the prefix.
		if _, err := ri.ResponseWriter.Write(b); err != nil {
			return 0, err
		}
		_, err := ri.ResponseWriter.Write(ri.prefix)
		return len(b), err
	}
	return ri.ResponseWriter.Write(b)
}

// findInjectionPoint returns the byte offset in b where the prefix should be
// inserted. It looks (case-insensitively) for:
//  1. "<!doctype html>" or "<!doctype html " — insert after the closing ">"
//  2. "<head>" or "<head " — insert after the closing ">"
//
// Returns -1 if no suitable marker is found.
func findInjectionPoint(b []byte) int {
	lower := strings.ToLower(string(b))

	// Try <!DOCTYPE html...> first.
	if idx := strings.Index(lower, "<!doctype"); idx >= 0 {
		// Find the closing '>'.
		if end := strings.IndexByte(lower[idx:], '>'); end >= 0 {
			return idx + end + 1
		}
	}

	// Fall back to <head...>.
	if idx := strings.Index(lower, "<head"); idx >= 0 {
		if end := strings.IndexByte(lower[idx:], '>'); end >= 0 {
			return idx + end + 1
		}
	}

	return -1
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

// Command cortex-tsnet is a userspace Tailscale sidecar for Cortex.
//
// It joins the tailnet using tailscale.com/tsnet (no root, no system
// daemon, no admin) and exposes a local SOCKS5 proxy whose dialer is the
// tsnet node's Dial. Cortex routes its home-service HTTP traffic (gateway +
// Ollama) through that proxy so requests resolve + route over the tailnet
// with MagicDNS handled tailnet-side.
//
// It speaks a tiny machine-readable status protocol on stdout: one JSON
// object per line (see README.md). Cortex's Rust side parses these lines to
// surface the login URL and the connected tailnet identity.
//
// Security: the auth key is NEVER logged or echoed. It arrives via --authkey
// or the TS_AUTHKEY env var and is handed straight to tsnet.
package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"log"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"syscall"
	"time"

	socks5 "github.com/things-go/go-socks5"
	"tailscale.com/tsnet"
)

// status is one line of the stdout status protocol. Exactly one of the
// optional fields is meaningful per State; see emit* helpers.
type status struct {
	State   string `json:"state"`             // needs-login | connected | error | starting
	URL     string `json:"url,omitempty"`     // login URL when State == needs-login
	IP      string `json:"ip,omitempty"`      // tailnet IPv4 when State == connected
	DNSName string `json:"dnsname,omitempty"` // MagicDNS name when State == connected
	Msg     string `json:"msg,omitempty"`     // human message when State == error
}

// emit writes one status line as JSON to stdout and flushes. stdout is the
// status channel; all human/debug logging goes to stderr so the two never mix.
func emit(s status) {
	b, err := json.Marshal(s)
	if err != nil {
		return
	}
	fmt.Fprintln(os.Stdout, string(b))
}

func defaultStateDir(hostname string) string {
	if d, err := os.UserConfigDir(); err == nil {
		return filepath.Join(d, "cortex", "tsnet", hostname)
	}
	return filepath.Join(os.TempDir(), "cortex-tsnet-"+hostname)
}

func main() {
	var (
		hostname = flag.String("hostname", "cortex", "tailnet hostname for this node")
		stateDir = flag.String("state-dir", "", "directory for tsnet state (default: per-user config dir)")
		authKey  = flag.String("authkey", "", "tailnet auth key (or env TS_AUTHKEY); optional, enables headless login")
		socks    = flag.String("socks", "127.0.0.1:1055", "local SOCKS5 listen address")
	)
	flag.Parse()

	// Auth key precedence: --authkey flag wins, else TS_AUTHKEY env. Never log it.
	key := *authKey
	if key == "" {
		key = os.Getenv("TS_AUTHKEY")
	}

	dir := *stateDir
	if dir == "" {
		dir = defaultStateDir(*hostname)
	}
	if err := os.MkdirAll(dir, 0o700); err != nil {
		emit(status{State: "error", Msg: fmt.Sprintf("cannot create state dir: %v", err)})
		os.Exit(1)
	}

	// All tsnet/library logging goes to stderr so stdout stays a clean status
	// channel. We additionally watch these log lines for the auth URL, which is
	// the only robust cross-version way tsnet surfaces it before LocalClient is
	// ready.
	log.SetOutput(os.Stderr)

	emit(status{State: "starting"})

	srv := &tsnet.Server{
		Hostname:  *hostname,
		Dir:       dir,
		AuthKey:   key,
		Ephemeral: false,
		// Route tsnet's own logs to stderr and scrape them for the auth URL.
		Logf: func(format string, args ...any) {
			line := fmt.Sprintf(format, args...)
			fmt.Fprintln(os.Stderr, line)
			if u := extractAuthURL(line); u != "" {
				emit(status{State: "needs-login", URL: u})
			}
		},
	}

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	// Up blocks until the node is authenticated + running, or ctx is canceled.
	// While it blocks (interactive login), the Logf scraper above emits the
	// needs-login URL. We also poll LocalClient status as a second source.
	go pollForAuthURL(ctx, srv)

	if _, err := srv.Up(ctx); err != nil {
		if ctx.Err() != nil {
			// Clean shutdown during login — not an error to surface loudly.
			_ = srv.Close()
			return
		}
		emit(status{State: "error", Msg: fmt.Sprintf("tailscale up failed: %v", err)})
		_ = srv.Close()
		os.Exit(1)
	}
	defer srv.Close()

	// Report the connected identity.
	emitConnected(ctx, srv)

	// SOCKS5 server whose dialer routes every connection through the tailnet.
	// MagicDNS resolution happens tailnet-side because we resolve via srv.Dial
	// (name + port handed to the tsnet node), so socks5h on the client side
	// keeps DNS off the local box.
	conf := socks5.NewServer(
		socks5.WithDial(func(ctx context.Context, network, addr string) (net.Conn, error) {
			return srv.Dial(ctx, network, addr)
		}),
		socks5.WithResolver(tsnetResolver{srv}),
		socks5.WithLogger(socks5.NewLogger(log.New(os.Stderr, "socks5: ", log.LstdFlags))),
	)

	ln, err := net.Listen("tcp", *socks)
	if err != nil {
		emit(status{State: "error", Msg: fmt.Sprintf("cannot bind SOCKS5 %s: %v", *socks, err)})
		os.Exit(1)
	}
	defer ln.Close()
	fmt.Fprintf(os.Stderr, "cortex-tsnet: SOCKS5 listening on %s\n", *socks)

	// Serve until killed; close the listener when ctx is canceled to unblock.
	go func() {
		<-ctx.Done()
		_ = ln.Close()
	}()
	if err := conf.Serve(ln); err != nil && ctx.Err() == nil {
		emit(status{State: "error", Msg: fmt.Sprintf("socks5 serve: %v", err)})
		os.Exit(1)
	}
}

// tsnetResolver defers name resolution to the tailnet node so MagicDNS names
// resolve tailnet-side. Returning a nil IP with the original context tells
// go-socks5 to pass the host through to the (tsnet) dialer unresolved.
type tsnetResolver struct{ srv *tsnet.Server }

func (tsnetResolver) Resolve(ctx context.Context, name string) (context.Context, net.IP, error) {
	// nil IP => go-socks5 dials by name via our WithDial (tsnet), which is what
	// we want for MagicDNS. We never resolve locally.
	return ctx, nil, nil
}

// extractAuthURL pulls a Tailscale login URL out of a tsnet log line.
func extractAuthURL(line string) string {
	const marker = "https://login.tailscale.com/"
	idx := strings.Index(line, marker)
	if idx < 0 {
		return ""
	}
	rest := line[idx:]
	// URL ends at the first whitespace.
	if sp := strings.IndexAny(rest, " \t\n\r"); sp >= 0 {
		rest = rest[:sp]
	}
	return strings.TrimRight(rest, ".,)")
}

// pollForAuthURL polls the LocalClient for an AuthURL while Up() is blocking.
// This is a second, structured source for the login URL in addition to the
// Logf scraper, in case the log format changes across tsnet versions.
func pollForAuthURL(ctx context.Context, srv *tsnet.Server) {
	lc, err := srv.LocalClient()
	if err != nil {
		return
	}
	t := time.NewTicker(500 * time.Millisecond)
	defer t.Stop()
	emitted := ""
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			st, err := lc.Status(ctx)
			if err != nil || st == nil {
				continue
			}
			if st.AuthURL != "" && st.AuthURL != emitted {
				emitted = st.AuthURL
				emit(status{State: "needs-login", URL: st.AuthURL})
			}
			// Once running, stop polling — emitConnected handles the rest.
			if st.BackendState == "Running" {
				return
			}
		}
	}
}

// emitConnected reports the tailnet IPv4 + MagicDNS name once the node is up.
func emitConnected(ctx context.Context, srv *tsnet.Server) {
	ip := ""
	dnsname := ""
	if lc, err := srv.LocalClient(); err == nil {
		// Give the backend a moment to populate the netmap.
		deadline := time.Now().Add(5 * time.Second)
		for time.Now().Before(deadline) {
			st, err := lc.Status(ctx)
			if err == nil && st != nil && st.Self != nil {
				dnsname = strings.TrimSuffix(st.Self.DNSName, ".")
				for _, a := range st.Self.TailscaleIPs {
					if a.Is4() {
						ip = a.String()
						break
					}
				}
				if ip != "" {
					break
				}
			}
			time.Sleep(200 * time.Millisecond)
		}
	}
	emit(status{State: "connected", IP: ip, DNSName: dnsname})
}

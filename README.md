# Dispatch Proxy

A SOCKS5 load balancing proxy to combine multiple internet connections into one. Works on Windows, Linux, and macOS. Written in Rust using the tokio async runtime.

It can also be used as a transparent proxy to load balance multiple SSH tunnels.

## Features

- **IPv4 and IPv6 support** - Works with both address families
- **Auto-detection** - Automatically detect interfaces with working internet connectivity
- **Weighted load balancing** - Configurable contention ratios for each interface
- **Tunnel mode** - Load balance SSH tunnels or other SOCKS proxies
- **Cross-platform** - Works on Windows, Linux, and macOS

## Installation

### From Source

Ensure that Rust is installed (https://rustup.rs/).

```sh
git clone https://github.com/extremecoders-re/go-dispatch-proxy.git
cd go-dispatch-proxy
cargo build --release
```

The binary will be available at `target/release/dispatch-proxy`.

### Pre-built Binaries

Download the latest binary for your platform from [releases](https://github.com/extremecoders-re/go-dispatch-proxy/releases).

## Usage

### 1 - Auto-detect mode (easiest)

The simplest way to use dispatch-proxy is with auto-detection. It will automatically find all interfaces with working internet connectivity:

```
$ ./dispatch-proxy --auto
 INFO Auto-detecting interfaces with internet connectivity...
 INFO Load balancer 1: 192.168.1.2 (en0), contention ratio: 1
 INFO Load balancer 2: 10.81.201.18 (en1), contention ratio: 1
 INFO Local server started on 127.0.0.1:8080
```

### 2 - Manual interface selection

For more control, you can manually specify which interfaces to use. First, list available interfaces:

```
$ ./dispatch-proxy --list
--- Listing the available addresses for dispatching
[+] en0, IPv4:192.168.1.2
[+] en0, IPv6:fe80::1
[+] en1, IPv4:10.81.201.18
```

Then start the proxy with the desired interfaces:

```
$ ./dispatch-proxy 192.168.1.2@3 10.81.201.18@2
 INFO Load balancer 1: 192.168.1.2, contention ratio: 3
 INFO Load balancer 2: 10.81.201.18, contention ratio: 2
 INFO Local server started on 127.0.0.1:8080
```

The contention ratio (after @) determines how connections are distributed. In the example above, out of 5 consecutive connections, 3 go to the first interface and 2 to the second.

### 3 - IPv6 addresses

IPv6 addresses are supported. Use bracket notation:

```
$ ./dispatch-proxy [fe80::1]@2 [2001:db8::1]@1
```

### 4 - Tunnel mode (SSH load balancing)

Load balance multiple SSH tunnels:

```
# First, setup SSH tunnels
$ ssh -D 127.0.0.1:7777 user@server1.com
$ ssh -D 127.0.0.1:7778 user@server2.com

# Then start dispatch-proxy in tunnel mode
$ ./dispatch-proxy --tunnel 127.0.0.1:7777 127.0.0.1:7778
```

For IPv6 tunnels:

```
$ ./dispatch-proxy --tunnel [::1]:7777@2 [::1]:7778@1
```

## Command Line Options

```
Usage: dispatch-proxy [OPTIONS] [ADDRESSES]...

Arguments:
  [ADDRESSES]...  Load balancer addresses (IP@ratio or host:port@ratio for tunnel mode)

Options:
      --lhost <LHOST>  The host to listen for SOCKS connections [default: 127.0.0.1]
      --lport <LPORT>  The local port to listen for SOCKS connections [default: 8080]
  -l, --list           Shows the available addresses for dispatching
  -t, --tunnel         Use tunnelling mode (transparent load balancing proxy)
  -q, --quiet          Disable logs
  -a, --auto           Auto-detect interfaces with working internet connectivity
  -h, --help           Print help
```

## How Auto-Detection Works

When using `--auto`, dispatch-proxy:

1. Enumerates all non-loopback network interfaces
2. Tests each interface by attempting to connect to Cloudflare DNS (1.1.1.1 for IPv4, 2606:4700:4700::1111 for IPv6)
3. Interfaces that successfully connect within 3 seconds are used as load balancers
4. All detected interfaces get a default contention ratio of 1

## Linux Support

On Linux in normal mode, dispatch-proxy uses the `SO_BINDTODEVICE` syscall to bind to the interface corresponding to the load balancer IPs. As a result, the binary must be run with `root` privilege or by giving it the necessary capabilities:

```
$ sudo ./dispatch-proxy
```

OR (Recommended)

```
$ sudo setcap cap_net_raw=eip ./dispatch-proxy
$ ./dispatch-proxy
```

Tunnel mode and auto-detection don't require root privilege.

## Cross-Compilation

```sh
# Compile for Linux x64
cargo build --release --target x86_64-unknown-linux-gnu

# Compile for Windows x64
cargo build --release --target x86_64-pc-windows-gnu

# Compile for macOS ARM64 (Apple Silicon)
cargo build --release --target aarch64-apple-darwin
```

## Credits

- [dispatch-proxy](https://github.com/Morhaus/dispatch-proxy): The original SOCKS5/HTTP load balancing proxy written in NodeJS.

## License

Licensed under MIT

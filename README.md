# Dispatch Proxy

A SOCKS5 load balancing proxy to combine multiple internet connections into one. Works on Windows, Linux, and macOS. Written in Rust using the tokio async runtime.

It can also be used as a transparent proxy to load balance multiple SSH tunnels.

## Rationale

The idea for this project came from [dispatch-proxy](https://github.com/Morhaus/dispatch-proxy) which is written in NodeJS. This Rust implementation provides a single portable binary with excellent performance and low resource usage.

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

### 1 - Load balance connections

The primary purpose of the tool is to combine multiple internet connections into one. For this we need to know the IP addresses of the interfaces we wish to combine. You can obtain the IP addresses using `ipconfig` (Windows), `ip a` (Linux), or `ifconfig` (macOS). Alternatively run `dispatch-proxy --list`.

```
$ ./dispatch-proxy --list
--- Listing the available addresses for dispatching
[+] en0, IPv4:192.168.1.2
[+] en1, IPv4:10.81.201.18
```

Start `dispatch-proxy` specifying the IP addresses of the load balancers. Optionally, along with the IP address you may also provide the contention ratio (after the @ symbol). If no contention ratio is specified, it defaults to 1.

### 2 - Load balance SSH tunnels

The tool can load balance multiple SSH tunnels. See Example 3 for usage.

### Example 1

SOCKS proxy running on localhost at default port. Contention ratio is specified.

```
$ ./dispatch-proxy 10.81.201.18@3 192.168.1.2@2
 INFO Load balancer 1: 10.81.201.18, contention ratio: 3
 INFO Load balancer 2: 192.168.1.2, contention ratio: 2
 INFO Local server started on 127.0.0.1:8080
```

Out of 5 consecutive connections, the first 3 are routed to `10.81.201.18` and the remaining 2 to `192.168.1.2`.

### Example 2

SOCKS proxy running on a different interface at a custom port. Contention ratio is not specified.

```
$ ./dispatch-proxy --lhost 192.168.1.2 --lport 5566 10.81.177.215 192.168.1.100
 INFO Load balancer 1: 10.81.177.215, contention ratio: 1
 INFO Load balancer 2: 192.168.1.100, contention ratio: 1
 INFO Local server started on 192.168.1.2:5566
```

The SOCKS server is started by default on `127.0.0.1:8080`. It can be changed using the `--lhost` and `--lport` options.

Now change the proxy settings of your browser, download manager etc to point to the above address (eg `127.0.0.1:8080`). Be sure to add this as a **SOCKS v5 proxy** and NOT as a HTTP/S proxy.

### Example 3

The tool can be used to load balance multiple SSH tunnels. In this mode, dispatch-proxy acts as a transparent load balancing proxy.

First, setup the tunnels:

```
$ ssh -D 127.0.0.1:7777 user@192.168.1.100
$ ssh -D 127.0.0.1:7778 user@192.168.1.101
```

Here we are setting up two SSH tunnels to remote hosts `192.168.1.100` and `192.168.1.101` on local ports `7777` and `7778` respectively.

Next, launch dispatch-proxy using the `--tunnel` argument:

```
$ ./dispatch-proxy --tunnel 127.0.0.1:7777 127.0.0.1:7778
```

Both the IP and port must be mentioned while specifying the load balancer addresses. Domain names also work:

```
$ ./dispatch-proxy --tunnel proxy1.com:7777 proxy2.com:7778
```

Optionally, the listening host, port and contention ratio can also be specified:

```
$ ./dispatch-proxy --lport 5555 --tunnel 127.0.0.1:7777@1 127.0.0.1:7778@3
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
  -h, --help           Print help
```

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

Tunnel mode doesn't require root privilege.

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

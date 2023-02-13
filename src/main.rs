use clap::Parser;

use prometheus_client::encoding::text::encode;
use prometheus_client::registry::Registry;

use tokio::net::TcpStream as TokioTcpStream;
use trust_dns_client::client::AsyncClient;
use trust_dns_client::proto::iocompat::AsyncIoTokioAsStd;
use trust_dns_client::tcp::TcpClientStream;
use warp::Filter;

use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::thread;

mod dnsmasq;
mod internet_check;
mod packet_monitor;

#[derive(Parser, Debug)] // requires `derive` feature
#[command(term_width = 0)] // Just to make testing across clap features easier
struct Args {
    /// Bind Address
    #[arg(long, default_value_t = ("127.0.0.1:9155").parse().unwrap())]
    bind_addr: SocketAddr,

    /// network interface to monitor
    #[arg(long, default_value_t = format!("en0"))]
    interface: String,

    /// BPF filter
    #[arg(long, default_value_t = format!(""))]
    bpf: String,

    /// DNS leases path
    #[arg(long, default_value_t = format!("/var/lib/misc/dnsmasq.leases"))]
    leases_path: String,

    /// dnsmasq host:port address
    #[arg(long, default_value_t = format!("127.0.0.1:53").parse().unwrap())]
    dnsmasq_addr: String,
    // /// Implicitly using `std::str::FromStr`
    // #[arg(short = 'O')]
    // optimization: Option<usize>,

    // /// Allow invalid UTF-8 paths
    // #[arg(short = 'I', value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
    // include: Option<std::path::PathBuf>,

    // /// Handle IP addresses
    // #[arg(long)]
    // bind: Option<std::net::IpAddr>,

    // /// Allow human-readable durations
    // #[arg(long)]
    // sleep: Option<humantime::Duration>,

    // /// Hand-written parser for tuples
    // #[arg(short = 'D', value_parser = parse_key_val::<String, i32>)]
    // defines: Vec<(String, i32)>,

    // /// Support enums from a foreign crate that don't implement `ValueEnum`
    // #[arg(
    //     long,
    //     default_value_t = foreign_crate::LogLevel::Info,
    //     value_parser = clap::builder::PossibleValuesParser::new(["info", "debug", "info", "warn", "error"])
    //         .map(|s| s.parse::<foreign_crate::LogLevel>().unwrap()),
    // )]
    // log_level: foreign_crate::LogLevel,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    println!("{:?}", args);

    let registry = Registry::default();
    let registry = Arc::new(RwLock::new(registry));

    let packet_monitor = packet_monitor::PacketMonitor::new();
    packet_monitor.register(&mut registry.write().unwrap());
    {
        let interface = args.interface.clone();
        // let bpf = args.bpf.clone();
        thread::spawn(move || {
            packet_monitor.run(&interface);
        });
    }

    let internet_check = internet_check::InternetCheck::new();
    internet_check.register(&mut registry.write().unwrap());
    thread::spawn(move || {
        internet_check.start();
    });

    let dnsmasq = dnsmasq::DnsMasq::new();
    dnsmasq.register(&mut registry.write().unwrap());

    let address = args.dnsmasq_addr.parse().unwrap();
    let (stream, sender) = TcpClientStream::<AsyncIoTokioAsStd<TokioTcpStream>>::new(address);
    let client = AsyncClient::new(stream, sender, None);
    let (client, bg) = client.await.expect("connection failed");
    tokio::spawn(bg);

    {
        let registry = registry.clone();

        let p1 = warp::path!("hello" / String).map(|name| format!("Hello, {}!", name));
        let p2 = warp::path!("metrics").then(move || {
            let registry = registry.clone();
            let mut client = client.clone();
            let leases_path = args.leases_path.clone();
            let dnsmasq = dnsmasq.clone();

            async move {
                dnsmasq.update_lease_metrics(&leases_path).await;
                dnsmasq.update_dns_metrics(&mut client).await;

                let mut buffer = String::new();
                encode(&mut buffer, &registry.as_ref().read().unwrap()).unwrap();

                buffer
            }
        });
        let hello = p1.or(p2);

        warp::serve(hello).run(args.bind_addr).await;
    }
}

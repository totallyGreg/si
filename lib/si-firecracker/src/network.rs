use crate::errors::FirecrackerError;
use futures::stream::TryStreamExt;
use netlink_packet_route::LinkMessage;
use netlink_packet_route::{
    link::{LinkAttribute, LinkExtentMask},
    AddressFamily,
};
use rtnetlink::new_connection;
use rtnetlink::Handle;
use rtnetlink::LinkHandle;
use rustix::fd::AsRawFd;
use std::net::Ipv4Addr;
use std::result;
use sysctl::Sysctl;
use tun_tap::Iface;
use tun_tap::Mode;

use netns_rs::NetNs;

const MASK: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 252);
const TAP_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 2);

type Result<T> = result::Result<T, FirecrackerError>;
struct FirecrackerNetwork;

impl FirecrackerNetwork {
    pub async fn prepare_network(id: u8) -> Result<()> {
        let (_, handle, _) = new_connection()?;
        // calculate network IP addresses
        let net_link_jailer_ip =
            Ipv4Addr::new(100u8, 65u8, (4 * id + 2) / 256u8, (4 * id + 2) % 256u8);
        let net_link_main_ip = Ipv4Addr::new(100, 65, (4 * id + 1) / 256, (4 * id + 1) % 256);

        // create jail ns
        let ns_name = format!("jailer-{id}");
        let ns = NetNs::new(&ns_name)?;
        ns.run(|_| async {
            // create tap
            Self::create_tun_tap_device(handle.clone(), format!("fc-${id}-tap0")).await
        })?;

        Self::create_veth_link(
            handle.clone(),
            format!("veth-jailer-{id}"),
            format!("veth-main-{id}"),
            ns,
            net_link_main_ip,
            net_link_jailer_ip,
        )
        .await?;

        // set iptables rules for namespace
        NetNs::get(&ns_name)?
            .run(|_| async { Self::set_iptables_rules(net_link_jailer_ip).await })?;
        Ok(())
    }

    async fn create_tun_tap_device(handle: Handle, name: String) -> Result<()> {
        Iface::new(&name, Mode::Tun)?;
        // add address to tap and bring it up
        if let Some(link) = Self::get_link_by_name(handle.clone(), name).await {
            Self::add_ip_to_link(handle.clone(), &link, TAP_IP).await?;
            Self::set_link_to_up(handle.clone(), &link).await?;
        }

        // Set the sysctl parameters
        let ipv4_proxy_arp_param = format!("net.ipv4.conf.{}.proxy_arp", &name);
        let ipv6_disable_ipv6_param = format!("net.ipv6.conf.{}.disable_ipv6", &name);
        Self::set_sysctl_value(&ipv4_proxy_arp_param, 1)?;
        Self::set_sysctl_value(&ipv6_disable_ipv6_param, 1)?;
        Ok(())
    }

    async fn create_veth_link(
        handle: Handle,
        host: String,
        jail: String,
        ns: NetNs,
        host_ip: Ipv4Addr,
        jail_ip: Ipv4Addr,
    ) -> Result<()> {
        // create veth peer
        handle.link().add().veth(host, jail).execute().await?;

        // get jail dev, add it to the ns and give it an ip
        if let Some(link) = Self::get_link_by_name(handle.clone(), jail).await {
            handle
                .link()
                .set(link.header.index)
                .setns_by_fd(ns.file().as_raw_fd())
                .execute()
                .await?;

            ns.run(|_| async {
                Self::add_ip_to_link(handle.clone(), &link, jail_ip).await?;
                Self::set_link_to_up(handle.clone(), &link).await?;
                // the jail veth should route to the host veth by default
                Self::set_default_route(handle.clone(), host_ip).await?;
                Ok(())
            })?;
        }

        // give the host dev an ip
        if let Some(link) = Self::get_link_by_name(handle.clone(), host).await {
            Self::add_ip_to_link(handle.clone(), &link, host_ip).await?;
            Self::set_link_to_up(handle.clone(), &link).await?;
        }
        Ok(())
    }

    async fn add_ip_to_link(handle: Handle, link: &LinkHandle, ip: Ipv4Addr) -> Result<()> {
        handle
            .address()
            .add(link.header.index, ip, ip.prefix())
            .execute()
            .await?;
        Ok(())
    }

    async fn get_link_by_name(handle: Handle, name: String) -> Option<LinkMessage> {
        handle
            .link()
            .get()
            .match_name(name)
            .execute()
            .try_next()
            .await?
    }

    async fn set_link_to_up(handle: Handle, link: &LinkHandle) -> Result<()> {
        link.set(link.header.index).up().execute().await?;
        Ok(())
    }

    fn set_sysctl_value(name: &str, value: i32) -> Result<()> {
        if let Ok(ctl) = sysctl::Ctl::new(name) {
            ctl.set_value(sysctl::CtlValue::Int(value))?;
        }
        Ok(())
    }

    async fn set_default_route(handle: Handle, ip: Ipv4Addr) -> Result<()> {
        handle
            .route()
            .add()
            .v4()
            .destination_prefix(ip, ip.prefix())
            .execute()
            .await?;
        Ok(())
    }

    async fn set_iptables_rules(ip: Ipv4Addr) -> Result<()> {
        let ipt = iptables::new(false)?;
        ipt.append("nat", "POSTROUTING", &format!("-o {} -j MASQUERADE", ip))?;
        ipt.append(
            "filter",
            "FORWARD",
            "-m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT",
        )?;
        // forward from the tuntap to the jailer veth
        ipt.append(
            "filter",
            "FORWARD",
            &format!("-i {} -o {} -j ACCEPT", TAP_IP, ip),
        )?;
        // drop traffic to the ec2 metadata endpoint
        ipt.append("filter", "OUTPUT", "-d 169.254.169.254 -j DROP")?;
        Ok(())
    }
}

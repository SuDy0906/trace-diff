#!/usr/bin/env bash
# Multi-hop Linux netns + tc netem testbed for trace-diff.
# Requires: root, iproute2, traceroute/ping utilities.
set -euo pipefail

NS_A=td-a
NS_B=td-b
NS_C=td-c

cleanup() {
  ip netns del "$NS_A" 2>/dev/null || true
  ip netns del "$NS_B" 2>/dev/null || true
  ip netns del "$NS_C" 2>/dev/null || true
}
trap cleanup EXIT

cleanup

ip netns add "$NS_A"
ip netns add "$NS_B"
ip netns add "$NS_C"

# A <-> B
ip link add veth-ab type veth peer name veth-ba
ip link set veth-ab netns "$NS_A"
ip link set veth-ba netns "$NS_B"

# B <-> C
ip link add veth-bc type veth peer name veth-cb
ip link set veth-bc netns "$NS_B"
ip link set veth-cb netns "$NS_C"

ip -n "$NS_A" addr add 10.200.1.1/24 dev veth-ab
ip -n "$NS_B" addr add 10.200.1.2/24 dev veth-ba
ip -n "$NS_B" addr add 10.200.2.1/24 dev veth-bc
ip -n "$NS_C" addr add 10.200.2.2/24 dev veth-cb

ip -n "$NS_A" link set lo up
ip -n "$NS_B" link set lo up
ip -n "$NS_C" link set lo up
ip -n "$NS_A" link set veth-ab up
ip -n "$NS_B" link set veth-ba up
ip -n "$NS_B" link set veth-bc up
ip -n "$NS_C" link set veth-cb up

# Enable forwarding on B (middle hop)
ip netns exec "$NS_B" sysctl -w net.ipv4.ip_forward=1 >/dev/null

ip -n "$NS_A" route add default via 10.200.1.2
ip -n "$NS_C" route add default via 10.200.2.1
ip -n "$NS_B" route add 10.200.1.0/24 dev veth-ba
ip -n "$NS_B" route add 10.200.2.0/24 dev veth-bc

# Inject delay/loss on A→B path
ip netns exec "$NS_A" tc qdisc add dev veth-ab root netem delay 40ms 5ms loss 5%

echo "Testbed ready:"
echo "  client ns: $NS_A (10.200.1.1)"
echo "  hop ns:    $NS_B (10.200.1.2 / 10.200.2.1)"
echo "  dest ns:   $NS_C (10.200.2.2)"
echo
echo "Example:"
echo "  sudo ip netns exec $NS_A ./target/debug/trace-diff run 10.200.2.2 --skip-http --output text"
echo
echo "Press Ctrl+C to tear down."
sleep infinity

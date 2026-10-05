#!/usr/bin/env python3
#   Copyright 2026 The Tari Project
#   SPDX-License-Identifier: BSD-3-Clause
"""Swarm smoke test: exercises the main transaction paths against a running local swarm.

Stages:
  1. 🚰 Faucet-claim flood via transaction_generator + transaction_submitter
  2. 📦 Publish freshly-built templates through the wallet daemon
  3. 🥷 Stealth transfers of tTARI between wallet accounts
  4. 🪙 Call a newly published template that mints a stealth token into the `smoketest` account,
     then stealth-transfer some of it
  5. 🖼️  Mint testnet NFTs from the builtin faucet

Ports are discovered from the swarm daemon (default http://localhost:8080) unless --wallet-url /
--indexer-url are given. The wallet daemon must use `None` auth.

    scripts/smoke_test.py                 # everything
    scripts/smoke_test.py --only templates,nfts
    scripts/smoke_test.py --flood 500 --templates 5
"""

import argparse
import base64
import json
import os
import re
import secrets
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TEMPLATE_DIR = REPO / "scripts" / "smoke_test_template"
TEMPLATE_WASM = TEMPLATE_DIR / "target" / "wasm32-unknown-unknown" / "release" / "smoke_test_template.wasm"
TARI_TOKEN = "resource_" + "01" * 32
TARI = 1_000_000
SMOKE_ACCOUNT = "smoketest"
STAGES = ["flood", "templates", "stealth", "token", "nfts"]

TEMPLATE_ADDR_RE = re.compile(r"^template_[0-9a-f]{64}$")
RESOURCE_ADDR_RE = re.compile(r"^resource_[0-9a-f]{64}$")

USE_COLOR = sys.stdout.isatty() and not os.environ.get("NO_COLOR")


def _c(code, s):
    return f"\033[{code}m{s}\033[0m" if USE_COLOR else s


def bold(s):
    return _c("1", s)


def dim(s):
    return _c("2", s)


def green(s):
    return _c("32", s)


def red(s):
    return _c("31", s)


def yellow(s):
    return _c("33", s)


def info(msg):
    print(f"   {msg}")


def detail(msg):
    print(dim(f"      {msg}"))


def short(addr):
    if isinstance(addr, str) and "_" in addr and len(addr) > 30:
        prefix, hexpart = addr.split("_", 1)
        return f"{prefix}_{hexpart[:8]}…{hexpart[-6:]}"
    return str(addr)


def fmt_tari(micro):
    return f"{int(micro) / TARI:,.6f}".rstrip("0").rstrip(".") + " tTARI"


class StageFailed(Exception):
    pass


# --------------------------------------------------------------------------------------------- RPC


class JsonRpc:
    def __init__(self, url, timeout=120):
        self.url = url
        self.timeout = timeout
        self.token = None
        self._id = 0

    def call(self, method, params=None, timeout=None):
        self._id += 1
        body = json.dumps({"jsonrpc": "2.0", "id": self._id, "method": method, "params": params or {}}).encode()
        headers = {"Content-Type": "application/json"}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        req = urllib.request.Request(self.url, data=body, headers=headers)
        try:
            with urllib.request.urlopen(req, timeout=timeout or self.timeout) as resp:
                payload = json.load(resp)
        except urllib.error.URLError as e:
            raise StageFailed(f"{method}: cannot reach {self.url}: {e}") from e
        if payload.get("error"):
            err = payload["error"]
            raise StageFailed(f"{method}: {err.get('message')} {json.dumps(err.get('data')) if err.get('data') else ''}")
        return payload["result"]


class Wallet(JsonRpc):
    def login(self):
        self.token = None
        self.token = self.call("auth.request", {"permissions": ["Admin"], "credentials": "None"})["token"]
        self.login_at = time.monotonic()

    def call(self, method, params=None, timeout=None):
        # JWTs expire after 5 minutes; a long flood would otherwise outlive the token.
        if self.token and method != "auth.request" and time.monotonic() - self.login_at > 240:
            self.login()
        return super().call(method, params, timeout)

    def wait(self, tx_id, timeout_secs=120):
        res = self.call(
            "transactions.wait_result",
            {"transaction_id": tx_id, "timeout_secs": timeout_secs},
            timeout=timeout_secs + 30,
        )
        if res.get("timed_out"):
            raise StageFailed(f"transaction {short(tx_id)} timed out after {timeout_secs}s")
        status = res.get("status")
        if status != "Accepted":
            raise StageFailed(f"transaction {short(tx_id)} finished as {status}: {reject_reason(res.get('result'))}")
        return res


def reject_reason(finalize):
    if not finalize:
        return "no result"
    found = find_key(finalize, "message") or find_key(finalize, "Reject") or find_key(finalize, "reason")
    if isinstance(found, str):
        return found
    return json.dumps(found)[:400] if found else json.dumps(finalize)[:400]


def find_key(obj, key):
    if isinstance(obj, dict):
        if key in obj:
            return obj[key]
        for v in obj.values():
            r = find_key(v, key)
            if r is not None:
                return r
    elif isinstance(obj, list):
        for v in obj:
            r = find_key(v, key)
            if r is not None:
                return r
    return None


def find_strings(obj, pattern):
    out = []
    if isinstance(obj, str):
        if pattern.match(obj):
            out.append(obj)
    elif isinstance(obj, dict):
        for k, v in obj.items():
            out += find_strings(k, pattern) + find_strings(v, pattern)
    elif isinstance(obj, list):
        for v in obj:
            out += find_strings(v, pattern)
    return list(dict.fromkeys(out))


def discover(args):
    if args.wallet_url and args.indexer_url:
        return args.wallet_url, args.indexer_url
    swarm = JsonRpc(f"{args.swarm_url.rstrip('/')}/json_rpc", timeout=10)
    instances = swarm.call("list_instances")["instances"]
    wallet_url, indexer_url = args.wallet_url, args.indexer_url
    for inst in instances:
        if not inst.get("is_running"):
            continue
        if not wallet_url and inst["instance_type"] == "TariWalletDaemon":
            wallet_url = f"http://127.0.0.1:{inst['ports']['jrpc']}/json_rpc"
        if not indexer_url and inst["instance_type"] == "TariIndexer":
            indexer_url = f"http://127.0.0.1:{inst['ports']['api']}"
    if not wallet_url or not indexer_url:
        raise StageFailed("swarm has no running wallet daemon and indexer; pass --wallet-url / --indexer-url")
    return wallet_url, indexer_url


# ------------------------------------------------------------------------------------------ helpers


def run(cmd, cwd=REPO, env=None, capture=True):
    proc = subprocess.run(
        cmd, cwd=cwd, env={**os.environ, **(env or {})}, text=True, capture_output=capture
    )
    if proc.returncode != 0:
        tail = "\n".join(((proc.stdout or "") + (proc.stderr or "")).strip().splitlines()[-15:])
        raise StageFailed(f"`{' '.join(map(str, cmd))}` exited {proc.returncode}\n{tail}")
    return proc.stdout or ""


def build_template(nonce):
    run(
        ["cargo", "build", "--release", "--target", "wasm32-unknown-unknown"],
        cwd=TEMPLATE_DIR,
        env={"SMOKE_TEST_NONCE": nonce},
    )
    return TEMPLATE_WASM.read_bytes()


def balances(wallet, account):
    res = wallet.call("accounts.get_balances", {"account": {"Name": account}, "refresh": True})
    return {b["resource_address"]: b for b in res["balances"]}


def revealed(entry):
    return int(entry["balance"]) if entry else 0


def total(entry):
    return int(entry["balance"]) + int(entry["confidential_balance"]) if entry else 0


# --------------------------------------------------------------------------------------------- stages


class SmokeTest:
    def __init__(self, args):
        self.args = args
        self.wallet = None
        self.indexer_url = None
        self.smoke = None  # create_or_get response for the smoketest account
        self.other = None  # a second account to receive stealth transfers
        self.template = None

    # ---- setup

    def setup(self):
        wallet_url, self.indexer_url = discover(self.args)
        self.wallet = Wallet(wallet_url)
        self.wallet.login()
        info(f"👛 wallet   {dim(wallet_url)}")
        info(f"🔎 indexer  {dim(self.indexer_url)}")

        self.smoke = self.wallet.call("accounts.create_or_get", {"account": {"Name": SMOKE_ACCOUNT}})
        verb = "created" if self.smoke["created"] else "found"
        info(f"🧪 account  {bold(SMOKE_ACCOUNT)} {verb} {dim(short(self.smoke['account']['component_address']))}")

        self.other = self.wallet.call("accounts.create_or_get", {"account": {"Name": f"{SMOKE_ACCOUNT}-recv"}})
        info(f"📥 account  {bold(SMOKE_ACCOUNT + '-recv')} {dim(short(self.other['account']['component_address']))}")

        bal = revealed(balances(self.wallet, SMOKE_ACCOUNT).get(TARI_TOKEN))
        if bal < self.args.min_balance * TARI:
            info(f"🚰 balance {fmt_tari(bal)} is low, claiming from the faucet…")
            res = self.wallet.call(
                "accounts.create_free_test_coins",
                {"account": {"Name": SMOKE_ACCOUNT}, "max_fee": 100_000},
                timeout=180,
            )
            detail(f"+{fmt_tari(res['amount'])} (fee {res['fee']} µT)")
            bal = revealed(balances(self.wallet, SMOKE_ACCOUNT).get(TARI_TOKEN))
        info(f"💰 balance  {fmt_tari(bal)}")

    # ---- 1

    def stage_flood(self):
        n = self.args.flood
        bins = REPO / "target" / "release"
        if not self.args.no_build:
            info("🔨 building transaction_generator + transaction_submitter…")
            run(["cargo", "build", "--release", "-p", "transaction_generator", "-p", "transaction_submitter"])
        with tempfile.TemporaryDirectory(prefix="smoke-") as tmp:
            out = Path(tmp) / "claims.bin"
            info(f"✍️  generating {bold(n)} faucet-claim transactions…")
            run([
                bins / "transaction_generator", "write",
                "-n", str(n), "-o", out, "--indexer-url", self.indexer_url,
            ])
            info(f"🚀 submitting to {dim(self.indexer_url)}…")
            started = time.monotonic()
            output = run([
                bins / "transaction_submitter", "stress-test",
                "-f", out, "-a", self.indexer_url, "-y",
            ])
            elapsed = time.monotonic() - started

        stats = {}
        for key, label in [
            ("submitted", "Transactions submitted"),
            ("committed", "Fully committed"),
            ("fee_only", "Fee charged, execution rejected"),
            ("rejected", "Rejected"),
            ("errored", "Errored"),
        ]:
            m = re.search(rf"{re.escape(label)}:\s*(\d+)", output)
            stats[key] = int(m.group(1)) if m else None
        if stats["committed"] is None:
            raise StageFailed("could not parse submitter summary:\n" + "\n".join(output.splitlines()[-15:]))

        detail(
            f"submitted {stats['submitted']} · committed {stats['committed']} · fee-only {stats['fee_only']} · "
            f"rejected {stats['rejected']} · errored {stats['errored']} · {elapsed:.1f}s "
            f"(~{stats['committed'] / elapsed:.1f} TPS)"
        )
        if stats["committed"] != n:
            raise StageFailed(f"only {stats['committed']}/{n} claims committed")
        return f"{n} claims committed in {elapsed:.1f}s"

    # ---- 2

    def stage_templates(self):
        count = self.args.templates
        published = []
        for i in range(count):
            nonce = secrets.token_hex(8)
            binary = build_template(nonce)
            res = self.wallet.call("transactions.publish_template", {
                "binary": base64.b64encode(binary).decode(),
                "fee_account": {"Name": SMOKE_ACCOUNT},
                "max_fee": 2 * TARI,
                "detect_inputs": True,
                "dry_run": False,
            })
            result = self.wallet.wait(res["transaction_id"])
            addrs = find_strings(result.get("result"), TEMPLATE_ADDR_RE)
            if not addrs:
                raise StageFailed(f"publish {res['transaction_id']} accepted but no template address in its diff")
            published.append(addrs[0])
            detail(f"📦 {i + 1}/{count} {short(addrs[0])} · {len(binary) // 1024} KiB · fee {result['final_fee']} µT")
        self.template = published[-1]
        return f"{count} template(s) published"

    # ---- 3

    def stealth_transfer(self, resource, amount, dest, label):
        res = self.wallet.call("accounts.stealth_transfer", {
            "owner_account": {"Name": SMOKE_ACCOUNT},
            "fee_params": {"input_selection": "PreferRevealed", "pay_fee_with_swap": None},
            "input_selection": "PreferRevealed",
            "resource_address": resource,
            "transfers": [{
                "destination_address": dest,
                "blinded_output_amount": amount,
                "revealed_output_amount": 0,
                "pay_to": "StealthPublicKey",
                "attach_sender_address": True,
            }],
            "max_fee": 200_000,
            "dry_run": False,
        })
        result = self.wallet.wait(res["transaction_id"])
        detail(f"🥷 {label} → {short(dest)} · fee {result['final_fee']} µT")

    def stage_stealth(self):
        resource = TARI_TOKEN
        dest = self.other["address"]
        before = total(balances(self.wallet, f"{SMOKE_ACCOUNT}-recv").get(resource))
        amounts = [TARI, 2 * TARI, 3 * TARI][: self.args.stealth]
        for a in amounts:
            self.stealth_transfer(resource, a, dest, fmt_tari(a))
        after = total(balances(self.wallet, f"{SMOKE_ACCOUNT}-recv").get(resource))
        if after - before != sum(amounts):
            raise StageFailed(f"recipient balance moved by {fmt_tari(after - before)}, expected {fmt_tari(sum(amounts))}")
        return f"{len(amounts)} stealth transfer(s), recipient +{fmt_tari(sum(amounts))}"

    # ---- 4

    def stage_token(self):
        if not self.template:
            info("📦 no template from this run, publishing one…")
            self.stage_templates_once()
        symbol = "SMK" + secrets.token_hex(2).upper()
        supply = 1_000_000
        account = self.smoke["account"]["component_address"]
        manifest = f"""
use {self.template} as SmokeToken;

fn fee_main() {{
    let account = var!["account"];
    account.pay_fee({TARI});
}}

fn main() {{
    let account = var!["account"];
    let coins = SmokeToken::mint("{symbol}", {supply});
    account.deposit(coins);
}}
"""
        res = self.wallet.call("transactions.submit_manifest", {
            "manifest": manifest,
            "variables": {"account": account},
            "seal_signer_key_id": self.smoke["account"]["owner_key_id"],
            "signing_key_ids": [],
            "max_fee": TARI,
            "dry_run": False,
            "blobs": {},
        })
        result = self.wallet.wait(res["transaction_id"])
        resources = [r for r in find_strings(result.get("result"), RESOURCE_ADDR_RE) if r != TARI_TOKEN]
        detail(f"🪙 minted {supply:,} {bold(symbol)} via {short(self.template)} · fee {result['final_fee']} µT")

        bals = balances(self.wallet, SMOKE_ACCOUNT)
        entry = next((b for b in bals.values() if b.get("token_symbol") == symbol), None)
        if entry is None:
            # The wallet may not have picked up the new stealth resource's metadata yet.
            entry = next((bals[r] for r in resources if r in bals), None)
        if entry is None or total(entry) != supply:
            raise StageFailed(f"{SMOKE_ACCOUNT} does not hold {supply} {symbol} (saw {entry})")
        token = entry["resource_address"]
        detail(f"💼 {SMOKE_ACCOUNT} holds {total(entry):,} {symbol} {dim(short(token))}")

        self.stealth_transfer(token, 1000, self.other["address"], f"1,000 {symbol}")
        recv = total(balances(self.wallet, f"{SMOKE_ACCOUNT}-recv").get(token))
        if recv != 1000:
            raise StageFailed(f"recipient holds {recv} {symbol}, expected 1000")
        self.expect_orphan_rejected()
        return f"{symbol} minted, deposited and stealth-transferred; orphan resource rejected"

    def expect_orphan_rejected(self):
        """A resource that ends the transaction referenced by no component or vault must not commit."""
        res = self.wallet.call("transactions.submit_manifest", {
            "manifest": f"""
use {self.template} as SmokeToken;
fn fee_main() {{
    let account = var!["account"];
    account.pay_fee({TARI});
}}
fn main() {{
    SmokeToken::create_orphan("ORPHAN");
}}
""",
            "variables": {"account": self.smoke["account"]["component_address"]},
            "seal_signer_key_id": self.smoke["account"]["owner_key_id"],
            "signing_key_ids": [],
            "max_fee": TARI,
            "dry_run": False,
            "blobs": {},
        })
        try:
            self.wallet.wait(res["transaction_id"])
        except StageFailed as e:
            if "orphaned substate" not in str(e):
                raise StageFailed(f"orphan-resource transaction failed for an unexpected reason: {e}") from e
            detail("🚫 orphan resource rejected: " + str(e).split(": ", 1)[-1][:200])
            return
        raise StageFailed("a transaction that creates an unreferenced (orphan) resource was accepted")

    def stage_templates_once(self):
        saved = self.args.templates
        self.args.templates = 1
        try:
            self.stage_templates()
        finally:
            self.args.templates = saved

    # ---- 5

    def stage_nfts(self):
        n = self.args.nfts
        res = self.wallet.call("nfts.mint_faucet_nft", {
            "account": {"Name": SMOKE_ACCOUNT},
            "mutable_data": {"name": f"Smoke test {time.strftime('%H:%M:%S')}", "image_url": "https://tari.com/favicon.ico"},
            "number_to_mint": n,
            "max_fee": 200_000,
        }, timeout=180)
        finalize = res.get("finalize") or {}
        if "Accept" not in json.dumps(finalize.get("result", finalize))[:200] and find_key(finalize, "Reject"):
            raise StageFailed(f"NFT mint rejected: {reject_reason(finalize)}")
        detail(f"🖼️  minted {n} NFT(s) · fee {res['fee']} µT · tx {short(res['transaction_id'])}")
        return f"{n} faucet NFT(s) minted"


# ----------------------------------------------------------------------------------------------- main

STAGE_TITLES = {
    "flood": "🚰 Faucet-claim flood",
    "templates": "📦 Publish templates",
    "stealth": "🥷 Stealth transfers",
    "token": "🪙 Mint stealth token from new template",
    "nfts": "🖼️  Mint faucet NFTs",
}


def main():
    p = argparse.ArgumentParser(description="Ootle swarm smoke test", formatter_class=argparse.RawDescriptionHelpFormatter,
                                epilog=__doc__)
    p.add_argument("--swarm-url", default="http://localhost:8080")
    p.add_argument("--wallet-url", help="walletd JSON-RPC URL, e.g. http://127.0.0.1:5100/json_rpc")
    p.add_argument("--indexer-url", help="indexer REST URL, e.g. http://127.0.0.1:12500")
    p.add_argument("--flood", type=int, default=200, help="faucet claims to flood (default 200)")
    p.add_argument("--templates", type=int, default=3, help="templates to publish (default 3)")
    p.add_argument("--stealth", type=int, default=3, choices=[1, 2, 3], help="tTARI stealth transfers (default 3)")
    p.add_argument("--nfts", type=int, default=3, help="faucet NFTs to mint (default 3)")
    p.add_argument("--min-balance", type=int, default=100, help="claim faucet coins below this many tTARI")
    p.add_argument("--only", help=f"comma-separated subset of: {','.join(STAGES)}")
    p.add_argument("--skip", help="comma-separated stages to skip")
    p.add_argument("--no-build", action="store_true", help="use existing target/release generator/submitter")
    p.add_argument("--fail-fast", action="store_true", help="stop at the first failing stage")
    args = p.parse_args()

    selected = args.only.split(",") if args.only else list(STAGES)
    skipped = set(args.skip.split(",")) if args.skip else set()
    for s in selected + list(skipped):
        if s not in STAGES:
            p.error(f"unknown stage {s!r}; choose from {', '.join(STAGES)}")
    selected = [s for s in STAGES if s in selected and s not in skipped]

    print(bold("\n🔥 Ootle swarm smoke test\n"))
    test = SmokeTest(args)
    started = time.monotonic()
    try:
        print(bold("⚙️  Setup"))
        test.setup()
    except StageFailed as e:
        print(red(f"   ❌ setup failed: {e}"))
        return 1

    results = []
    for i, stage in enumerate(selected, 1):
        print(bold(f"\n[{i}/{len(selected)}] {STAGE_TITLES[stage]}"))
        t0 = time.monotonic()
        try:
            summary = getattr(test, f"stage_{stage}")()
            dt = time.monotonic() - t0
            print(green(f"   ✅ {summary}") + dim(f"  ({dt:.1f}s)"))
            results.append((stage, True, summary, dt))
        except StageFailed as e:
            dt = time.monotonic() - t0
            print(red(f"   ❌ {e}") + dim(f"  ({dt:.1f}s)"))
            results.append((stage, False, str(e).splitlines()[0], dt))
            if args.fail_fast:
                break
        except KeyboardInterrupt:
            print(yellow("\n   ⏹️  interrupted"))
            results.append((stage, False, "interrupted", time.monotonic() - t0))
            break

    passed = sum(ok for _, ok, _, _ in results)
    print(bold("\n📋 Summary"))
    for stage, ok, summary, dt in results:
        mark = "✅" if ok else "❌"
        print(f"   {mark} {STAGE_TITLES[stage]:<42} {dim(f'{dt:6.1f}s')}  {summary if ok else red(summary)}")
    for stage in selected[len(results):]:
        print(f"   ⏭️  {STAGE_TITLES[stage]:<42} {dim('   not run')}")
    total_s = time.monotonic() - started
    if passed == len(selected):
        print(green(bold(f"\n🎉 All {passed} stages passed in {total_s:.1f}s\n")))
        return 0
    print(red(bold(f"\n💥 {len(selected) - passed}/{len(selected)} stages failed ({total_s:.1f}s)\n")))
    return 1


if __name__ == "__main__":
    sys.exit(main())

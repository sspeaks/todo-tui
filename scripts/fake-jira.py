#!/usr/bin/env python3
"""A fake Jira Data Center for scripts/smoke.sh: /myself, discovery and
search over two synthetic issues, on 127.0.0.1, and the writes a push
makes (ADR-0007): PUT /issue/{id} and BRME's transitions (Open, In
Progress (2), and Closed, which requires a resolution). Each write is
applied to its issue, so a later pull reads it back, and appended to
argv[2] as a JSON line. Prints its port, then serves until killed. Only
the bearer token in argv[1] is accepted.

Usage: fake-jira.py TOKEN WRITES_LOG
"""

import json
import re
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TOKEN = sys.argv[1]
WRITES = sys.argv[2] if len(sys.argv) > 2 else None
BASE = ""
CLOCK = [10]


def issue(n, title, category, status):
    return {
        "id": str(n),
        "key": f"PLAT-{n}",
        "fields": {
            "summary": title,
            "description": None,
            "issuetype": {"name": "Task"},
            "status": {"name": status, "statusCategory": {"key": category}},
            "priority": {"name": "Major"},
            "assignee": {"key": "JIRAUSER10100", "name": "dana", "displayName": "Dana Example"},
            "labels": [],
            "components": [],
            "fixVersions": [],
            "created": "2026-10-01T09:00:00.000+0000",
            "updated": f"2026-10-0{n}T09:00:00.000+0000",
        },
    }


ISSUES = [
    issue(1, "smoke jira login fix", "new", "Open"),
    issue(2, "smoke jira report", "indeterminate", "In Review"),
]
# Wiki markup, read as Markdown (ADR-0009); the colour has no Markdown form.
ISSUES[1]["fields"]["description"] = "h2. Report\r\n* weekly {{totals}}\r\n{color:#888}footer{color}"

# BRME's workflow: (id, name, target status, its category).
WORKFLOW = [
    ("61", "Open", "Open", "new"),
    ("91", "In Progress (2)", "In Progress", "indeterminate"),
    ("71", "Closed", "Closed", "done"),
]
RESOLUTIONS = [{"id": "10000", "name": "Done"}, {"id": "10101", "name": "Won't Do"}]


def touched(issue):
    """Bumps `updated`, as Jira does on each write."""
    CLOCK[0] += 1
    issue["fields"]["updated"] = f"2026-10-{CLOCK[0]:02d}T09:00:00.000+0000"


def log(entry):
    if WRITES:
        with open(WRITES, "a") as f:
            f.write(json.dumps(entry) + "\n")


GET = {
    "/rest/api/2/myself": {"key": "JIRAUSER10100", "name": "dana", "displayName": "Dana Example", "timeZone": "UTC"},
    "/rest/api/2/priority": [{"name": n} for n in ["Blocker", "Critical", "Major", "Minor", "Trivial"]],
    "/rest/api/2/field": [{"id": "summary", "name": "Summary", "custom": False}],
}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, code, body):
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def authorized(self):
        if self.headers.get("Authorization") == f"Bearer {TOKEN}":
            return True
        self.reply(401, {"errorMessages": ["unauthorized"]})
        return False

    def do_GET(self):
        if not self.authorized():
            return
        path = self.path.split("?")[0]
        if path in GET:
            return self.reply(200, GET[path])
        m = re.fullmatch(r"/rest/api/2/issue/([A-Z0-9-]+)(/transitions)?", path)
        # By id or by key, as Jira takes either (attach reads by key).
        found = [i for i in ISSUES if m and m.group(1) in (i["id"], i["key"])]
        if found and m.group(2):
            transitions = []
            for tid, name, to, key in WORKFLOW:
                t = {"id": tid, "name": name, "to": {"name": to, "statusCategory": {"key": key}}, "fields": {}}
                if key == "done":
                    t["fields"]["resolution"] = {"required": True, "name": "Resolution", "allowedValues": RESOLUTIONS}
                transitions.append(t)
            return self.reply(200, {"transitions": transitions})
        if found:
            return self.reply(200, found[0])
        self.reply(404, {"errorMessages": ["Issue Does Not Exist"]})

    def no_content(self):
        self.send_response(204)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def do_PUT(self):
        if not self.authorized():
            return
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")
        m = re.fullmatch(r"/rest/api/2/issue/(\d+)", self.path.split("?")[0])
        found = [i for i in ISSUES if m and i["id"] == m.group(1)]
        if not found:
            return self.reply(404, {"errorMessages": ["Issue Does Not Exist"]})
        issue = found[0]
        for k, v in body.get("fields", {}).items():
            issue["fields"][k] = v
        touched(issue)
        log({"method": "PUT", "key": issue["key"], "fields": body.get("fields", {})})
        self.no_content()

    def do_POST(self):
        if not self.authorized():
            return
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")
        if self.path.split("?")[0] == "/rest/api/2/issue":
            # Promote (design 5.7): a new issue in PLAT, the next id.
            f = body.get("fields", {})
            if f.get("project", {}).get("key") != "PLAT":
                return self.reply(400, {"errorMessages": [], "errors": {"issuetype": "valid issue type is required"}})
            n = max(int(i["id"]) for i in ISSUES) + 1
            new = issue(n, f.get("summary", ""), "new", "Open")
            for k in ("description", "labels", "duedate"):
                if f.get(k) is not None:
                    new["fields"][k] = f[k]
            touched(new)
            ISSUES.append(new)
            log({"method": "POST", "key": new["key"], "created": f.get("summary", "")})
            return self.reply(201, {"id": new["id"], "key": new["key"]})
        m = re.fullmatch(r"/rest/api/2/issue/(\d+)/transitions", self.path.split("?")[0])
        if m:
            found = [i for i in ISSUES if i["id"] == m.group(1)]
            chosen = [w for w in WORKFLOW if w[0] == body.get("transition", {}).get("id")]
            if not found or not chosen:
                return self.reply(400, {"errorMessages": ["not a valid workflow operation"]})
            issue, (tid, name, to, key) = found[0], chosen[0]
            wanted = body.get("fields", {}).get("resolution") or {}
            resolution = [r for r in RESOLUTIONS if r["id"] == wanted.get("id") or r["name"] == wanted.get("name")]
            if key == "done" and not resolution:
                return self.reply(400, {"errorMessages": [], "errors": {"resolution": "Resolution is required."}})
            issue["fields"]["status"] = {"name": to, "statusCategory": {"key": key}}
            issue["fields"]["resolution"] = resolution[0] if key == "done" else None
            touched(issue)
            log({"method": "POST", "key": issue["key"], "to": to, "resolution": resolution[0]["name"] if resolution else None})
            return self.no_content()
        if self.path.split("?")[0] != "/rest/api/2/search":
            return self.reply(404, {"errorMessages": ["no such resource"]})
        # Data Center's POST /search reads validateQuery as a boolean only.
        if not isinstance(body.get("validateQuery", True), bool):
            return self.reply(400, {"errorMessages": ["Cannot deserialize value of type `java.lang.Boolean`"]})
        jql = body.get("jql", "")
        after = re.search(r"id > (\d+)", jql)
        wanted = re.search(r"id in \(([^)]*)\)", jql)
        rows = ISSUES
        if after:
            rows = [i for i in rows if int(i["id"]) > int(after.group(1))]
        if wanted:
            ids = {s.strip() for s in wanted.group(1).split(",")}
            rows = [i for i in rows if i["id"] in ids]
        start = int(body.get("startAt", 0))
        page = rows[start : start + 100]
        self.reply(200, {"startAt": start, "maxResults": 100, "total": len(rows), "issues": page})


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
print(server.server_address[1], flush=True)
server.serve_forever()

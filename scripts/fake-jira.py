#!/usr/bin/env python3
"""A fake Jira Data Center for scripts/smoke.sh: /myself, discovery and
search over two synthetic issues, on 127.0.0.1. Prints its port, then
serves until killed. Only the bearer token in argv[1] is accepted.

Usage: fake-jira.py TOKEN
"""

import json
import re
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TOKEN = sys.argv[1]
BASE = ""


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
        m = re.fullmatch(r"/rest/api/2/issue/(\d+)", path)
        found = [i for i in ISSUES if m and i["id"] == m.group(1)]
        if found:
            return self.reply(200, found[0])
        self.reply(404, {"errorMessages": ["Issue Does Not Exist"]})

    def do_POST(self):
        if not self.authorized():
            return
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")
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

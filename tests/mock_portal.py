#!/usr/bin/env python3
"""M2 用的"假 AC"（模拟宿舍门户），只监听 127.0.0.1:18080。

为什么需要它：
真实的宿舍门户一旦登录成功，没法从本机安全地"注销"（`/eportal/portal/logout`
会回 `注销操作非本机终端！`），所以没法在不影响用户上网的前提下反复验证
"掉线 → 自动重登"。这里用一个行为等价的迷你门户，把状态机所有分支都跑一遍。

模拟行为（全部对应实测到的真实响应格式）：
- `/a79.htm`                  → 200（宿舍门户"可达"的判据）
- `/eportal/portal/page/loadConfig` → `login_method=1, account_suffix=@campus`
- `/eportal/portal/login`     → 账号不带 @campus 时回"无法获取用户认证账号！"（复现旧 bug）
- `/204`                      → 已认证 204，未认证 503（判在线/离线）
- `/__drop`                   → 模拟被注销
- `/__state`                  → 统计（登录次数、见过的账号）
- `/__set_password`           → 改期望密码（测"密码错→暂停"）
"""

import json
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import parse_qs, urlparse

STATE = {
    "authed": False,
    "logins": 0,
    "loadconfig": 0,
    "account_seen": [],
    "password": "secret-test-pw",
}
LOCK = threading.Lock()


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):  # 安静
        pass

    def _send(self, code, body, ctype="application/json; charset=utf-8"):
        data = body.encode("utf-8") if isinstance(body, str) else body
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        try:
            self.wfile.write(data)
        except Exception:
            pass

    def do_GET(self):
        u = urlparse(self.path)
        q = parse_qs(u.query)
        path = u.path

        if path == "/204":
            with LOCK:
                ok = STATE["authed"]
            return self._send(204, b"") if ok else self._send(503, b"")

        if path == "/captive":
            # 复现真实 AC 行为：未认证 → 劫持成登录页（200 + HTML）；认证后 → 204 空响应
            with LOCK:
                authed = STATE["authed"]
            if authed:
                return self._send(204, b"")
            return self._send(
                200,
                "<html><head><title>上网登录</title></head><body>"
                '<form action="/eportal/portal/login">校园网认证</form></body></html>',
                "text/html",
            )

        if path == "/__auth":
            with LOCK:
                STATE["authed"] = True
            return self._send(200, '{"ok":true}')

        if path == "/a79.htm":
            return self._send(200, "<html><body>portal</body></html>", "text/html")

        if path.endswith("/page/loadConfig"):
            with LOCK:
                STATE["loadconfig"] += 1
            return self._send(
                200,
                'jsonpReturn({"code":1,"msg":"加载页面设置信息成功！","data":'
                '{"login_method":"1","account_suffix":"@campus",'
                '"find_mac":"0","en_md5":"0","page_style":"standard"}});',
            )

        if path.endswith("/eportal/portal/login"):
            account = q.get("user_account", q.get("DDDDD", [""]))[0]
            password = q.get("user_password", q.get("upass", [""]))[0]
            with LOCK:
                STATE["account_seen"].append(account)
                expected = STATE["password"]
            if "@campus" not in account:
                # 复现旧方案的 bug：账号少了后缀
                return self._send(
                    200,
                    'jsonpReturn({"result":0,"msg":"无法获取用户认证账号！","ret_code":1});',
                )
            if password != expected:
                return self._send(
                    200, 'jsonpReturn({"result":0,"msg":"密码错误","ret_code":1});'
                )
            with LOCK:
                STATE["logins"] += 1
                STATE["authed"] = True
            return self._send(
                200, 'jsonpReturn({"result":1,"msg":"认证成功","ret_code":0});'
            )

        if path.endswith("/online_list"):
            with LOCK:
                ok = STATE["authed"]
            if not ok:
                return self._send(200, 'jsonpReturn({"result":1,"list":[]});')
            return self._send(
                200,
                json.dumps(
                    {
                        "result": 1,
                        "list": [
                            {
                                "user_account": "20250000",
                                "online_ip": "10.126.83.10",
                                "online_mac": "001122334455",
                                "online_time": "2026-11-12 10:00:00",
                                "time_long": "60",
                                "online_session": "1",
                            }
                        ],
                    }
                ),
            )

        if path == "/__drop":
            with LOCK:
                STATE["authed"] = False
            return self._send(200, '{"ok":true}')

        if path == "/__state":
            with LOCK:
                return self._send(200, json.dumps(STATE, ensure_ascii=False))

        if path == "/__set_password":
            with LOCK:
                STATE["password"] = q.get("value", [""])[0]
            return self._send(200, '{"ok":true}')

        return self._send(404, "not found", "text/plain")


if __name__ == "__main__":
    print("mock portal on http://127.0.0.1:18080", flush=True)
    HTTPServer(("127.0.0.1", 18080), Handler).serve_forever()

"""產生 assets/THIRD-PARTY-LICENSES.txt：編進 exe 的每個 crate 的授權全文。

相依有變（cargo add / cargo update）就重跑：python gen_licenses.py
只收真的會連結進 exe 的：一般相依，排除 proc-macro 與只在建置時用的套件。
內容一字不差的授權檔合併成一段，列出共用它的套件 —— 不然同一份 Apache 全文會重複兩百次。
雙授權（MIT OR Apache-2.0 之類）選 MIT 那份：短，而且帶著作者的著作權行。
"""
import json
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(ROOT, "assets", "THIRD-PARTY-LICENSES.txt")
TARGET = "x86_64-pc-windows-msvc"

# 不是 crate、但也編進 exe 的東西
EXTRA = [
    ("Butterchurn", "2.6.7", "MIT", "https://github.com/jberg/butterchurn",
     "視覺效果的 preset 引擎移植自 Butterchurn（shader 轉換與 preset 參數語意）。",
     "MIT License\n\nCopyright (c) 2013-2018 Jordan Berg"),
    ("butterchurn-presets", "2.4.7", "MIT", "https://github.com/jberg/butterchurn-presets",
     "內建的 preset 資料取自 butterchurn-presets。",
     "MIT License\n\nCopyright (c) 2013-2018 Jordan Berg"),
    ("OpenCC 字典資料", "（隨 ferrous-opencc 0.4.0）", "Apache-2.0", "https://github.com/BYVoid/OpenCC",
     "歌詞簡轉繁用的字典，經 ferrous-opencc 編進 exe。",
     "Copyright (c) BYVoid and OpenCC contributors\n\nApache License 2.0，全文見文末。"),
]
MIT_BODY = """Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE."""

LICENSE_FILE = re.compile(r"^(LICEN[CS]E|COPYING|COPYRIGHT|NOTICE|UNLICENSE)([-_.].*)?$", re.I)


def metadata():
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--filter-platform", TARGET],
        cwd=ROOT, capture_output=True, check=True).stdout
    return json.loads(out)


def linked_packages(d):
    pk = {p["id"]: p for p in d["packages"]}
    nodes = {n["id"]: n for n in d["resolve"]["nodes"]}
    root = d["resolve"]["root"]
    seen, stack = set(), [root]
    while stack:
        i = stack.pop()
        if i in seen:
            continue
        seen.add(i)
        for dep in nodes[i]["deps"]:
            if any(k["kind"] is None for k in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    seen.discard(root)
    keep = []
    for i in seen:
        p = pk[i]
        if any("proc-macro" in t["kind"] for t in p["targets"]):
            continue
        keep.append(p)
    return sorted(keep, key=lambda p: (p["name"].lower(), p["version"]))


def pick_files(p):
    """這個套件要附的授權檔。雙授權只取 MIT 那份；NOTICE 一律附上（Apache 要求）。"""
    d = os.path.dirname(p["manifest_path"])
    files = sorted(f for f in os.listdir(d) if LICENSE_FILE.match(f) and os.path.isfile(os.path.join(d, f)))
    expr = p.get("license") or ""
    notices = [f for f in files if f.upper().startswith("NOTICE")]
    lic = [f for f in files if f not in notices]
    if " OR " in expr or "/" in expr:
        mit = [f for f in lic if "MIT" in f.upper()]
        if mit:
            lic = mit
        elif len(lic) > 1:
            # 沒有 MIT 檔：GPL 那份絕對不選（self_cell 是 Apache-2.0 OR GPL-2.0-only），
            # 其餘避開 Apache 長文，留短的（BSL、Zlib、ISC…）
            lic = [f for f in lic if "GPL" not in f.upper()] or lic
            other = [f for f in lic if "APACHE" not in f.upper()]
            lic = other or lic
    return [os.path.join(d, f) for f in lic + notices]


def read(path):
    with open(path, encoding="utf-8", errors="replace") as f:
        t = f.read()
    return t.replace("\r\n", "\n").replace("\r", "\n").strip("\n")


def main():
    d = metadata()
    pkgs = linked_packages(d)
    groups = {}     # 授權文字 -> [套件]
    order = []
    missing = []
    for p in pkgs:
        files = pick_files(p)
        if not files:
            missing.append(p)
            continue
        text = "\n\n".join(read(f) for f in files)
        if text not in groups:
            groups[text] = []
            order.append(text)
        groups[text].append(p)

    gpl = [p for p in pkgs if "GPL" in (p.get("license") or "") and " OR " not in p["license"]]
    if gpl:
        sys.exit("有只能用 GPL 授權的套件，編進去整個 exe 就得改用 GPL，不能維持 MIT：%s"
                 % ", ".join(p["name"] for p in gpl))

    rule = "-" * 64
    out = [
        "VitroDeck —— 第三方軟體授權",
        "=" * 64,
        "",
        "VitroDeck.exe 是用 Rust 編譯的單一執行檔，裡面含有下列開源元件。",
        "以下列出各元件的授權條款，以符合其散布時的姓名標示要求。",
        "",
        "VitroDeck 本身的著作權與這些元件無關，各元件的權利屬於其各自的作者。",
        "有多種授權可選的元件（例如 MIT OR Apache-2.0），本程式選用下文所附的那一種；",
        "可選 GPL 的元件一律選用其非 GPL 的授權。",
        "",
        "圖形驅動、媒體控制等 Windows 系統元件由使用者的作業系統提供，",
        "未包含在本執行檔內。",
        "",
        "共 %d 個元件。" % (len(pkgs) + len(EXTRA)),
        "=" * 64,
    ]
    for name, ver, lic, src, note, head in EXTRA:
        out += ["", "", rule, "%s %s" % (name, ver), "授權：%s" % lic, "來源：%s" % src, note, rule, "",
                head + "\n\n" + MIT_BODY]
    for text in order:
        ps = groups[text]
        out += ["", "", rule]
        for p in ps:
            out.append("%s %s（%s）" % (p["name"], p["version"], p.get("license") or "見下文"))
        srcs = sorted({p.get("repository") or "" for p in ps} - {""})
        if len(srcs) == 1:
            out.append("來源：%s" % srcs[0])
        out += [rule, "", text]
    if missing:
        out += ["", "", rule, "下列元件的套件裡沒有附授權檔，依其宣告的授權條款使用：", rule, ""]
        for p in missing:
            out.append("%s %s：%s  %s" % (p["name"], p["version"], p.get("license") or "?",
                                         p.get("repository") or ""))
        out += ["", "MIT License 全文：", "", MIT_BODY]
    # 沒附檔的、以及 OpenCC 字典，都要有 Apache-2.0 全文；從某個附了全文的 crate 借
    apache = next((read(os.path.join(os.path.dirname(p["manifest_path"]), f))
                   for p in pkgs for f in sorted(os.listdir(os.path.dirname(p["manifest_path"])))
                   if re.match(r"^LICEN[CS]E[-_.]APACHE", f, re.I)), None)
    if not apache:
        sys.exit("找不到 Apache-2.0 全文")
    out += ["", "", rule, "Apache License 2.0 全文", rule, "", apache]
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, "w", encoding="utf-8", newline="\r\n") as f:
        f.write("\n".join(out) + "\n")
    print("%d 個 crate，%d 段授權文字，%d 個沒附授權檔 -> %s"
          % (len(pkgs), len(order), len(missing), OUT))
    for p in missing:
        print("  沒附授權檔：", p["name"], p["version"], p.get("license"))


if __name__ == "__main__":
    main()

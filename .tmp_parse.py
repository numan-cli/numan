import json, sys
sys.stdout.reconfigure(encoding='utf-8')
data = json.load(open('.tmp_threads.json', encoding='utf-8'))
threads = data['data']['repository']['pullRequest']['reviewThreads']['nodes']
unresolved = [t for t in threads if not t['isResolved']]
print(f"Total unresolved: {len(unresolved)}\n")
for t in unresolved:
    c = t['comments']['nodes'][0]
    body_short = c['body'][:120].replace('\n', ' ')
    print(f"{t['id']}  outdated={t['isOutdated']}  {t['path']}:{t.get('line','?')}  [{c['author']['login']}]  {body_short}")

#!/usr/bin/env python3
"""Exercise the real CLI against synthetic logs in an isolated home directory."""
import argparse
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT/'dist/ccusage')
    args = parser.parse_args()
    binary = str(args.binary.resolve())
    with tempfile.TemporaryDirectory(prefix='ccusage-dsh-smoke-') as directory:
        home = Path(directory)
        dsh = home/'.dsh'
        for session, version in [('parent',4),('child',4),('fork',4),('legacy',3),('copy-parent',4)]:
            source = 'parent' if session == 'copy-parent' else session
            target = dsh/'sessions'/'project'/session/f'session.v{version}.jsonl.zstd'
            target.parent.mkdir(parents=True)
            shutil.copy2(ROOT/f'tests/fixtures/{source}.v{version}.zstd',target)
        # v3 alongside v4 must not add usage a second time.
        shutil.copy2(ROOT/'tests/fixtures/legacy.v3.zstd',dsh/'sessions/project/parent/session.v3.jsonl.zstd')
        claude = home/'.claude/projects/synthetic/session.jsonl'
        claude.parent.mkdir(parents=True)
        claude.write_text(json.dumps({'type':'assistant','version':'1.2.3','timestamp':'2026-10-25T15:00:00Z','sessionId':'claude-existing',
            'requestId':'synthetic-request','costUSD':0.25,'message':{'id':'synthetic-message','model':'synthetic-claude',
            'usage':{'input_tokens':7,'output_tokens':8}}}, separators=(',',':'))+'\n')
        devin_home = home/'devin-home'
        devin_db = devin_home/'cli'/'sessions.db'
        devin_db.parent.mkdir(parents=True)
        devin_metrics = [
            # Streaming snapshots of the same request count once.
            ('devin-alpha', 'req-1', 100, 10, 40, None, 1793600000),
            ('devin-alpha', 'req-1', 100, 10, 40, None, 1793600005),
            ('devin-alpha', 'req-2', 200, 20, None, 5, 1793600100),
            ('devin-beta', 'req-3', 60, 6, None, None, 1793680000),
        ]
        with sqlite3.connect(devin_db) as db:
            db.execute("CREATE TABLE sessions (id TEXT PRIMARY KEY, working_directory TEXT, model TEXT, created_at INTEGER, last_activity_at INTEGER)")
            db.execute("CREATE TABLE message_nodes (row_id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT, node_id INTEGER, parent_node_id INTEGER, chat_message TEXT, created_at INTEGER)")
            db.execute("INSERT INTO sessions VALUES ('devin-alpha','/work/alpha','swe-2-max',1793600000,1793600200)")
            db.execute("INSERT INTO sessions VALUES ('devin-beta','/work/beta','swe-2-high',1793680000,1793680200)")
            for node_id, (session, request, input_t, output_t, cache_read, cache_create, created) in enumerate(devin_metrics, 1):
                metrics = {'input_tokens':input_t,'output_tokens':output_t,'cache_read_tokens':cache_read,'cache_creation_tokens':cache_create}
                message = {'message_id':f'msg-{request}','role':'assistant','content':'ok',
                           'metadata':{'request_id':request,'generation_model':'swe-2-max','metrics':metrics}}
                db.execute("INSERT INTO message_nodes (session_id,node_id,chat_message,created_at) VALUES (?,?,?,?)",
                           (session, node_id, json.dumps(message, separators=(',',':')), created))
            # Non-assistant and metric-less rows are ignored.
            db.execute("INSERT INTO message_nodes (session_id,node_id,chat_message,created_at) VALUES ('devin-alpha',99,?,1793600200)",
                       (json.dumps({'message_id':'u1','role':'user','content':'hi','metadata':{}}),))
        env = {'PATH':os.environ['PATH'],'HOME':str(home),'DSH_HOME':str(dsh),'DEVIN_HOME':str(devin_home),'LOG_LEVEL':'0','NO_COLOR':'1',
               'CLAUDE_CONFIG_DIR':str(home/'.claude'),'CODEX_HOME':str(home/'.codex'),
               'XDG_DATA_HOME':str(home/'.local/share'),'XDG_CONFIG_HOME':str(home/'.config')}
        def run(*argv):
            return subprocess.check_output([binary,*argv],env=env,cwd=home,text=True,stderr=subprocess.PIPE)
        assert 'dsh' in run('--help')
        assert 'devin' in run('--help')
        assert '+dsh.devin.1' in run('--version')
        # No subcommand: this is the primary user-facing contract.
        table = run()
        assert 'dsh' in table.lower(), table
        assert 'devin' in table.lower(), table
        for report, key in [('daily','daily'),('weekly','weekly'),('monthly','monthly'),('session','sessions')]:
            data = json.loads(run('dsh',report,'--json','--offline','--timezone','Asia/Tokyo'))
            totals = data['totals']
            assert totals['inputTokens'] == 143, data
            assert totals['outputTokens'] == 33, data
            assert totals['cacheReadTokens'] == 900, data
            assert totals['totalTokens'] == 1076, data
            assert totals['totalCost'] == 0, data
            assert sum(row['totalTokens'] for row in data[key]) == 1076
        devin = json.loads(run('devin','daily','--json','--offline','--timezone','Asia/Tokyo'))
        assert devin['totals']['inputTokens'] == 360, devin
        assert devin['totals']['outputTokens'] == 36, devin
        assert devin['totals']['cacheReadTokens'] == 40, devin
        assert devin['totals']['cacheCreationTokens'] == 5, devin
        assert devin['totals']['totalTokens'] == 441, devin
        assert devin['totals']['totalCost'] == 0, devin
        sessions = json.loads(run('devin','session','--json','--offline'))
        assert len(sessions['sessions']) == 2, sessions
        unified = json.loads(run('--json','--by-agent','--offline','--timezone','Asia/Tokyo','--mode','display'))
        assert unified['totals']['totalTokens'] == 1532, unified
        assert unified['totals']['totalCost'] == 0.25, unified
        filtered = json.loads(run('dsh','daily','--json','--offline','--since','20261026','--until','20261026','--timezone','Asia/Tokyo'))
        assert filtered['totals']['totalTokens'] == 1076, filtered
        excluded = json.loads(run('dsh','daily','--json','--offline','--since','20261027'))
        assert not excluded['daily'], excluded
        no_cost = json.loads(run('dsh','daily','--json','--offline','--no-cost'))
        assert 'totalCost' not in no_cost['totals'], no_cost
        # The installer preserves relative npm symlinks and can restore them.
        bindir = home/'bin';bindir.mkdir()
        old = bindir/'original';old.write_text('synthetic previous installation')
        (bindir/'ccusage').symlink_to('original')
        for extra in [[],['--uninstall']]:
            subprocess.run(['python3',str(ROOT/'scripts/install.py'),'--bin-dir',str(bindir),*extra],check=True,stdout=subprocess.PIPE)
        assert (bindir/'ccusage').is_symlink()
        assert os.readlink(bindir/'ccusage') == 'original'
        print('PASS: plain ccusage, four DSH reports, Devin sessions/dedupe/$0, legacy/migration, retry, fork/copy dedupe, existing Claude totals, filters, --no-cost, install/rollback')

if __name__ == '__main__':
    main()

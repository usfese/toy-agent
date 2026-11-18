import atexit, http.server, json, os, pty, select, signal, struct, subprocess, tempfile, termios, fcntl, threading, time
binary=os.path.abspath('target/debug/toy-agent')
events=[]; events_lock=threading.Lock()
command='printf tool-ok #\u202e'
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self,*a): pass
    def do_POST(self):
        data=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        assert self.headers.get('Authorization') == 'Bearer test'
        messages=data['messages']
        assert data['model']=='test-model'
        if '你是工具调用审查角色' in messages[0]['content']:
            assert 'tools' not in data or data['tools']==[], data.get('tools')
            assert len(messages)==2 and messages[1]['role']=='user'
            review=json.loads(messages[1]['content'])
            assert review['tool_call']=={'id':'a1','name':'bash','arguments':{'command':command}}
            assert any(m['role']=='user' and m['content']=='  hello\n' for m in review['history'])
            with events_lock:
                assert events==['assistant'], events
                events.append('reviewer')
            message={'role':'assistant','content':json.dumps({'decision':'allowed'})}
            reason='stop'
        elif messages[-1]['role']=='user':
            assert messages[-1]['content'] == '  hello\n'
            with events_lock:
                assert events==[], events
                events.append('assistant')
            message={'role':'assistant','content':None,'tool_calls':[{'id':'a1','type':'function','function':{'name':'bash','arguments':json.dumps({'command':command})}}]}
            reason='tool_calls'
        else:
            assert messages[-1]['role']=='tool' and messages[-1]['tool_call_id']=='a1'
            assert 'tool-ok' in messages[-1]['content'], messages[-1]
            with events_lock:
                assert events==['assistant','reviewer','approval'], events
                events.append('assistant_after_tool')
            message={'role':'assistant','content':'# Heading\n\n**bold**\n\n'+'\n\n'.join(f'line-{i:02}' for i in range(60))}
            reason='stop'
        body=json.dumps({'choices':[{'message':message,'finish_reason':reason}]}).encode();self.send_response(200);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
s=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);threading.Thread(target=s.serve_forever,daemon=True).start()
with tempfile.TemporaryDirectory() as tmp:
    config = os.path.join(tmp, 'config.json')
    with open(config, 'w') as f: json.dump({'client': {'api_key': 'test'}}, f)
    env={**os.environ,'TERM':'xterm-256color','TOY_AGENT_CONFIG':config,'TOY_AGENT_SESSIONS':tmp,'OPENAI_API_KEY':'ignored','OPENAI_MODEL':'test-model','OPENAI_BASE_URL':f'http://127.0.0.1:{s.server_port}/v1'}
    master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',24,90,0,0))
    original_termios = termios.tcgetattr(slave)
    proc=subprocess.Popen([binary],stdin=slave,stdout=slave,stderr=slave,env=env)
    atexit.register(lambda: proc.kill() if proc.poll() is None else None)
    output=b''; offset=0
    def expect(text, timeout=10):
        global output,offset
        needle=text.encode(); deadline=time.monotonic()+timeout
        while True:
            i=output.find(needle,offset)
            if i>=0: offset=i+len(needle); return
            if time.monotonic()>deadline: raise AssertionError(f'missing {text!r}; tail: {output[-2500:]!r}')
            if select.select([master],[],[],.2)[0]:
                try: output+=os.read(master,65536)
                except OSError: raise AssertionError(f'exited {proc.poll()}; tail: {output[-2500:]!r}')
    def send(data): os.write(master,data.encode())
    expect('test-model'); expect('❯ ')
    start = offset
    send('z'); expect('z')
    assert '❯'.encode() not in output[start:offset], 'input prompt was repainted while typing'
    send('\x1b[200~first\n第二\x1b[201~'); expect('第二')
    send('\x03'); send('\x1b[200~  hello\n\x1b[201~\r'); expect('Allow? [y/N]')
    assert b'\\u{202e}' in output, 'bidirectional control is not visibly escaped'
    with events_lock:
        assert events==['assistant','reviewer'], events
        send('y\n')
        events.append('approval')
    expect('Heading');expect('bold');expect('line-59');expect('test-model')
    with events_lock:
        assert events==['assistant','reviewer','approval','assistant_after_tool'], events
    assert output.count(b'\x1b[?1049h')==1, output.count(b'\x1b[?1049h')
    assert b'\x1b[?1049l' not in output, 'left alternate screen during request'
    send('\x1b[5~'); expect('line-')
    send('\x1b[6~'); expect('line-')
    send('!printf shell-ok\r');expect('shell-ok');expect('test-model')
    assert b'\x1b[?1049l' not in output, 'left alternate screen during shell command'
    send('/quit\r');proc.wait(timeout=10)
    while select.select([master],[],[],0)[0]:
        try: output+=os.read(master,65536)
        except OSError: break
    assert proc.returncode==0,proc.returncode
    assert 'π'.encode() not in output, 'unexpected Pi branding'
    assert output.count(b'\x1b[?1049l')==1,output.count(b'\x1b[?1049l')
    assert termios.tcgetattr(slave) == original_termios, 'terminal settings not restored'
    os.close(slave); os.close(master)
s.shutdown();print('PASS: one alternate-screen entry/exit, no page switch during model/tool/shell, long Markdown output and interactive approval')

"""Native complete-panel observation matches the CPU-owned state and migration."""
import json
import socket
import struct
import time

import pytest

from db2010.paths import ROOT
from test_display_first_burst import prepare_channel5
from test_native_checkpoint import engine, stopped, command, migration


def panel(vm, name):
    return json.loads(command(vm, 'qom-get', {'path':'/machine', 'property':name}))


@pytest.mark.parametrize('variant', ['single_frame', 'stream_nonzero', 'late_unknown'])
def test_cached_complete_panel_and_metadata_match_native_observation(tmp_path, variant):
    prepared=tmp_path/'config'; prepared.mkdir()
    path,_=prepare_channel5(ROOT, None, prepared,
        'black' if variant=='single_frame' else variant,
        frame=True, stream=variant!='single_frame')
    checkpoint=tmp_path/'display.vmstate'
    with engine(None, tmp_path/'source', prepared=path) as vm:
        before=panel(vm,'display-cached')
        assert before['known_pixels']==0 and before['pixels_hex']==''
        stopped(vm)
        expected=panel(vm,'display')
        assert panel(vm,'display-cached')==expected
        assert panel(vm,'display-status')=={k:v for k,v in expected.items() if k!='pixels_hex'}
        layout=panel(vm,'checkpoint-layout')
        migration(vm,'migrate',checkpoint)
    state=json.loads(path.read_text()); state['checkpoint_tcm']=layout
    path.write_text(json.dumps(state))
    with engine(None, tmp_path/'restored', prepared=path, incoming=True) as vm:
        migration(vm,'migrate-incoming',checkpoint)
        assert panel(vm,'display-cached')==expected
        assert panel(vm,'display-status')=={k:v for k,v in expected.items() if k!='pixels_hex'}


def streamed_panel(peer):
    def exact(length):
        data = bytearray()
        while len(data) < length:
            chunk = peer.recv(length - len(data))
            assert chunk, 'LCD stream ended inside an envelope'
            data.extend(chunk)
        return bytes(data)

    magic, version, format_id, width, height, length, revision = struct.unpack(
        '>4sHHHHIQ', exact(24))
    assert (magic, version, format_id, width, height, length) == (
        b'K750', 1, 1, 176, 220, 176 * 220 * 2)
    return revision, exact(length)


@pytest.mark.parametrize('variant', ['black', 'stream_nonzero', 'late_unknown'])
def test_binary_stream_matches_completed_panel_and_restores_while_paused(tmp_path, variant):
    prepared = tmp_path/'config'
    prepared.mkdir()
    path, _ = prepare_channel5(ROOT, None, prepared, variant, frame=True, stream=True)
    checkpoint = tmp_path/'display.vmstate'
    peer, output = socket.socketpair()
    with peer, output:
        # Force partial writes: observing/stopping the VM must still work
        # while a client has not consumed a single pixel.
        output.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 4096)
        peer.settimeout(5)
        with engine(None, tmp_path/'source', prepared=path,
                pass_fds=(output.fileno(),),
                extra_args=('-chardev', f'socket,id=lcd,fd={output.fileno()}')) as vm:
            assert command(vm, 'qom-get', {'path':'/machine', 'property':'display-stream'}) is True
            stopped(vm)
            expected = panel(vm, 'display-cached')
            before = panel(vm, 'summary')
            if expected['known_pixels'] == 176 * 220:
                revision, pixels = streamed_panel(peer)
                assert revision == expected['frames']
                assert pixels == bytes.fromhex(expected['pixels_hex'])
            else:
                peer.settimeout(.05)
                with pytest.raises(TimeoutError):
                    peer.recv(24)
            assert panel(vm, 'summary') == before
            layout = panel(vm, 'checkpoint-layout')
            migration(vm, 'migrate', checkpoint)
    state = json.loads(path.read_text())
    state['checkpoint_tcm'] = layout
    path.write_text(json.dumps(state))
    peer, output = socket.socketpair()
    with peer, output:
        peer.settimeout(5)
        with engine(None, tmp_path/'restored', prepared=path, incoming=True,
                pass_fds=(output.fileno(),),
                extra_args=('-chardev', f'socket,id=lcd,fd={output.fileno()}')) as vm:
            migration(vm, 'migrate-incoming', checkpoint)
            assert panel(vm, 'display-cached') == expected
            if expected['known_pixels'] == 176 * 220:
                revision, pixels = streamed_panel(peer)
                assert revision == expected['frames']
                assert pixels == bytes.fromhex(expected['pixels_hex'])
            assert command(vm, 'query-status')['running'] is False
            restored = panel(vm, 'summary')
            if before.get('stop', {}).get('reason') == 'instruction_limit':
                # Restoring a research boundary clears that session's limit.
                assert restored == {k:v for k,v in before.items() if k != 'stop'}
            else:
                assert restored == before

"""Native repeated EOF ownership and the fully drained ring reset contract."""
import copy
import json
import struct
import time

import pytest

from support.mma_playback_fixtures import make as playback_state, stream, started, source_packets, SOURCE, POLL, ACK, CLEAR
from support.fa_wire10_fixtures import frame
from support.mma_mode_fixtures import mode
from support.fixtures import qemu_run
from test_native_checkpoint import engine, stopped, command, migration, comparable

PROFILE = 'wire20-modexp1024-mma-drain-peer'
EOF = frame([0x0804,6,2,0,1,0,0,0,20])
FLUSH = frame([0x0704,20])
UNREGISTER = frame([0x0104,20])
RECEIVE = POLL[:20]+ACK+CLEAR


def make(steps, raw):
    state=playback_state(steps,raw)
    state['dsp_peer']['profile']=PROFILE
    return state


@pytest.mark.parametrize('late',[False,True])
def test_repeated_empty_eof_returns_each_word_and_completes_once(tmp_path,late):
    raw,base=stream()
    if late:
        steps=base+EOF+POLL[:20]+CLEAR+RECEIVE
    else:
        first=next(i for i,s in enumerate(base) if s==EOF[0] and base[i:i+len(EOF)]==frame([0x0804,6,2,1,1,0,0,0,20]))
        steps=base[:first]+EOF+POLL[:20]+CLEAR+base[first:]
    ordinary=qemu_run(make(base,raw),tmp_path/'single',trace=False)['dsp_peer']
    result=qemu_run(make(steps,raw),tmp_path/'repeat',trace=False)
    assert result['stop']['reason']=='instruction_limit',result['stop']
    p=result['dsp_peer']
    for key in ('audio_pcm_fnv1a64','audio_frames_played','audio_frames_decoded','audio_nonzero_samples'):
        assert p[key]==ordinary[key]
    assert p['audio_words_credited']==ordinary['audio_words_credited']+6
    assert p['audio_completed'] and not p['audio_active']
    assert p['audio_completions_acked']==1
    assert p['ring4_buffered_words']==p['audio_pending_credit']==0
    assert p['audio_credits_offered']==p['audio_credits_acked']


def test_drained_flush_returns_zero_then_unregisters_without_application_reply(tmp_path):
    raw,base=stream()
    reply=qemu_run(make(base+FLUSH+POLL[:20]+CLEAR,raw),tmp_path/'flush',trace=False)
    assert reply['stop']['reason']=='instruction_limit'
    p=reply['dsp_peer']
    assert struct.unpack('<32H',bytes.fromhex(p['rx_mailbox_hex']))[:4]==(0x180a,0x0204,0,20)
    assert p['audio_rx_kind']==1 and p['audio_completions_acked']==1
    result=qemu_run(make(base+FLUSH+RECEIVE+mode(1)+POLL[:20]+CLEAR+FLUSH+RECEIVE+UNREGISTER+POLL[:20]+CLEAR+mode(4)+RECEIVE,raw),tmp_path/'cleanup',trace=False)
    assert result['stop']['reason']=='instruction_limit',result['stop']
    p=result['dsp_peer']
    assert p['mma_mode']==4 and not p['ring4_registered']
    assert not p['rx_mailbox_ready'] and p['audio_rx_kind']==0
    assert p['audio_completions_acked']==1
    assert p['audio_credits_offered']==p['audio_credits_acked']


def test_repeated_empty_flush_preserves_the_pending_reply_and_releases_tx(tmp_path):
    raw, base = stream()
    pending = base + FLUSH + POLL[:20] + CLEAR
    ordinary = qemu_run(make(pending, raw), tmp_path/'single-flush', trace=False)
    repeated = pending + FLUSH + POLL[:20] + CLEAR + FLUSH + POLL[:20] + CLEAR
    result = qemu_run(make(repeated, raw), tmp_path/'repeated-flush', trace=False)
    assert result['stop']['reason'] == 'instruction_limit', result['stop']
    before, after = ordinary['dsp_peer'], result['dsp_peer']
    assert after['rx_mailbox_hex'] == before['rx_mailbox_hex']
    assert after['rx_mailbox_ready'] and after['audio_rx_kind'] == 1
    assert after['audio_credits_offered'] == before['audio_credits_offered']
    assert after['requests'] == before['requests'] + 2
    assert after['mailbox_events'] == before['mailbox_events'] + 2
    assert not after['drain_tx_pending'] and not result['fa_mailbox_tx']['busy']
    cleanup = repeated + ACK + CLEAR + mode(1) + POLL[:20] + CLEAR + UNREGISTER + POLL[:20] + CLEAR + mode(4) + RECEIVE
    result = qemu_run(make(cleanup, raw), tmp_path/'cleanup', trace=False)
    assert result['stop']['reason'] == 'instruction_limit', result['stop']
    assert not result['dsp_peer']['ring4_registered']
    assert result['dsp_peer']['audio_credits_offered'] == result['dsp_peer']['audio_credits_acked']


def test_unregister_preserves_pending_zero_credit_reply(tmp_path):
    raw, base = stream()
    pending = base + mode(1) + POLL[:20] + CLEAR + FLUSH + POLL[:20] + CLEAR
    ordinary = qemu_run(make(pending, raw), tmp_path/'pending-reply', trace=False)
    steps = pending + UNREGISTER + POLL[:20] + CLEAR
    result = qemu_run(make(steps, raw), tmp_path/'unregister', trace=False)
    assert result['stop']['reason'] == 'instruction_limit', result['stop']
    before, after = ordinary['dsp_peer'], result['dsp_peer']
    assert after['rx_mailbox_hex'] == before['rx_mailbox_hex']
    assert after['rx_mailbox_ready'] and after['audio_rx_kind'] == 1
    assert after['audio_credits_offered'] == before['audio_credits_offered']
    assert not after['ring4_registered'] and not after['drain_tx_pending']
    assert not result['fa_mailbox_tx']['busy']
    cleanup = steps + ACK + CLEAR + mode(4) + RECEIVE + mode(3) + RECEIVE
    result = qemu_run(make(cleanup, raw), tmp_path/'reopen', trace=False)
    assert result['stop']['reason'] == 'instruction_limit', result['stop']
    assert result['dsp_peer']['mma_mode'] == 3


@pytest.mark.parametrize('control', [FLUSH, UNREGISTER])
def test_pending_drain_tx_and_rx_survive_checkpoint(tmp_path, control):
    raw, base = stream()
    prefix = base + mode(1) + POLL[:20] + CLEAR + FLUSH + POLL[:20] + CLEAR + control
    steps = prefix + POLL[:20] + ACK + CLEAR
    state = make(steps, raw)
    state['dsp_peer']['latency_ns'] = 512
    state['record_diagnostics'] = False
    with engine(state, tmp_path/'whole') as vm:
        whole = stopped(vm)
    first = copy.deepcopy(state)
    first['instruction_limit'] = sum(s[1]*2+2 if s[0]=='delay' else 4 for s in prefix)
    saved = tmp_path/'flush.vmstate'
    with engine(first, tmp_path/'prefix') as vm:
        start = stopped(vm)
        assert start['dsp_peer']['drain_tx_pending']
        assert start['fa_mailbox_tx']['busy'] and start['dsp_peer']['rx_mailbox_ready']
        layout = json.loads(command(vm, 'qom-get', {'path':'/machine', 'property':'checkpoint-layout'}))
        migration(vm, 'migrate', saved)
    continuation = copy.deepcopy(state)
    continuation['instruction_limit'] -= first['instruction_limit']
    continuation['checkpoint_tcm'] = layout
    with engine(continuation, tmp_path/'restored', incoming=True) as vm:
        migration(vm, 'migrate-incoming', saved)
        restored = vm.snapshot(time.monotonic()+5)
        assert restored['dsp_peer'] == start['dsp_peer']
        assert restored['fa_mailbox_tx'] == start['fa_mailbox_tx']
        final = stopped(vm)
    assert comparable(final) == comparable(whole)


@pytest.mark.parametrize('control',[FLUSH,UNREGISTER])
def test_active_or_unconfigured_drain_stays_guarded(tmp_path,control):
    raw,base=started()
    result=qemu_run(make(base+RECEIVE+control,raw),tmp_path/'active',trace=False)
    assert result['stop']['reason']=='dsp_peer_audio_drain_lifecycle_unmodeled'


def test_encoded_data_after_eof_stays_guarded(tmp_path):
    raw,base=stream()
    more,transfers=source_packets(SOURCE.read_bytes()[:500])
    # Reuse the first500 bytes at the original fixture DMA address.
    assert raw.startswith(more)
    result=qemu_run(make(base+transfers[0],raw),tmp_path/'after-eof',trace=False)
    assert result['stop']['reason']=='dsp_peer_audio_data_after_eof_unmodeled'
    assert result['dsp_peer']['audio_completions_acked']==1


@pytest.mark.parametrize('phase',['late_eof','completion_reply','flush_reply'])
def test_pending_late_eof_credit_and_zero_flush_reply_migrate(tmp_path,phase):
    raw,base=stream()
    prefix=base[:-len(ACK+CLEAR)] if phase=='completion_reply' else base+(EOF+POLL[:20]+CLEAR if phase=='late_eof' else FLUSH+POLL[:20]+CLEAR)
    steps=prefix+RECEIVE
    state=make(steps,raw);state['record_diagnostics']=False
    with engine(state,tmp_path/'whole')as vm:whole=stopped(vm)
    first=copy.deepcopy(state)
    first['instruction_limit']=sum(s[1]*2+2 if s[0]=='delay' else 4 for s in prefix)+100
    saved=tmp_path/'drain.vmstate'
    with engine(first,tmp_path/'prefix')as vm:
        start=stopped(vm)
        assert start['dsp_peer']['audio_rx_kind']==(2 if phase=='completion_reply' else 1)
        layout=json.loads(command(vm,'qom-get',{'path':'/machine','property':'checkpoint-layout'}))
        migration(vm,'migrate',saved)
    continuation=copy.deepcopy(state)
    continuation['instruction_limit']-=first['instruction_limit']
    continuation['checkpoint_tcm']=layout
    with engine(continuation,tmp_path/'resumed',incoming=True)as vm:
        migration(vm,'migrate-incoming',saved)
        assert vm.snapshot(time.monotonic()+5)['dsp_peer']==start['dsp_peer']
        final=stopped(vm)
    assert comparable(final)==comparable(whole)
    assert final['dsp_peer']['audio_completions_acked']==1


@pytest.mark.parametrize('mode_value',[3,4])
def test_unrecovered_active_mode_changes_stay_guarded(tmp_path,mode_value):
    raw,base=started()
    result=qemu_run(make(base+RECEIVE+mode(mode_value),raw),tmp_path/'mode',trace=False)
    assert result['stop']['reason']=='dsp_peer_audio_active_control_unmodeled'

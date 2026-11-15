"""Native host timing, including old icount saves and ordinary clock saves."""
import copy
import json
import subprocess
import time

import pytest

from test_native_checkpoint import command, engine, migration
from test_native_time import sleeping_peripheral_state
from test_display_first_burst import prepare_channel5
from db2010.paths import ROOT
from support.fixtures import base_state, freeze_state, words


def property(vm, name):
    return json.loads(command(vm, 'qom-get', {'path':'/machine', 'property':name}))


@pytest.mark.parametrize('profile,guards,icount', [
    ({'profile':'qemu-virtual-v1', 'icount_shift':6}, True, None),
    ({'profile':'qemu-virtual-v1', 'icount_shift':6}, False, 'shift=5'),
    ({'profile':'qemu-realtime-v1'}, True, None),
    ({'profile':'qemu-realtime-v1'}, False, 'shift=6'),
])
def test_native_and_research_clocks_reject_mismatched_launches(tmp_path, profile, guards, icount):
    state = sleeping_peripheral_state()
    state.pop('instruction_limit')
    state['timebase'] = profile
    prepared = freeze_state(state, tmp_path)
    argv = [str(ROOT/'qemu/build/qemu-system-arm'), '-machine',
        f'db2010,research-guards={"on" if guards else "off"},state={prepared}',
        '-accel', 'tcg,thread=single', '-nodefaults', '-display', 'none',
        '-monitor', 'none', '-serial', 'none', '-S']
    if icount is not None: argv += ['-icount', icount]
    result = subprocess.run(argv, capture_output=True, text=True, timeout=10)
    assert result.returncode != 0
    assert 'requires qemu-virtual-v1 / icount shift6' in result.stderr


def test_legacy_native_save_keeps_time_peripherals_and_pause_on_realtime_clock(tmp_path):
    state = sleeping_peripheral_state()
    state.pop('instruction_limit')
    state['record_diagnostics'] = False
    checkpoint = tmp_path/'legacy.vmstate'
    with engine(state, tmp_path/'legacy', research_guards=False,
            icount='shift=6,sleep=on,align=off') as vm:
        command(vm, 'cont')
        time.sleep(.04)
        command(vm, 'stop')
        before = property(vm, 'summary')
        registers = vm.snapshot(time.monotonic()+5)['registers']
        layout = property(vm, 'checkpoint-layout')
        assert before['virtual_ns'] > 0 and registers[2] == 1
        migration(vm, 'migrate', checkpoint)

    realtime = copy.deepcopy(state)
    realtime['timebase'] = {'profile':'qemu-realtime-v1'}
    realtime['checkpoint_tcm'] = layout
    next_checkpoint = tmp_path/'realtime.vmstate'
    with engine(realtime, tmp_path/'realtime', incoming=True,
            research_guards=False, icount=None) as vm:
        migration(vm, 'migrate-incoming', checkpoint)
        restored = property(vm, 'summary')
        assert restored['virtual_ns'] == before['virtual_ns']
        assert restored['counter_kind'] == 'qemu-virtual-time'
        assert restored['observations'] is None
        assert vm.snapshot(time.monotonic()+5)['registers'] == registers
        time.sleep(.025)
        assert property(vm, 'summary')['virtual_ns'] == restored['virtual_ns']
        start = time.monotonic()
        command(vm, 'cont')
        time.sleep(.04)
        command(vm, 'stop')
        wall = time.monotonic() - start
        after = property(vm, 'summary')
        guest = (after['virtual_ns'] - restored['virtual_ns']) / 1e9
        assert .5 < guest / wall < 1.5
        assert after['timer_ticks'] >= restored['timer_ticks']
        migration(vm, 'migrate', next_checkpoint)

    with engine(realtime, tmp_path/'again', incoming=True,
            research_guards=False, icount=None) as vm:
        migration(vm, 'migrate-incoming', next_checkpoint)
        assert property(vm, 'summary')['virtual_ns'] == after['virtual_ns']
        assert command(vm, 'query-status')['running'] is False


def test_native_display_completes_without_instruction_count_slices(tmp_path):
    prepared = tmp_path/'config'
    prepared.mkdir()
    path, _ = prepare_channel5(ROOT, None, prepared, 'stream_nonzero', frame=True, stream=True)
    state = json.loads(path.read_text())
    state.pop('instruction_limit')
    state['timebase'] = {'profile':'qemu-realtime-v1'}
    state['record_diagnostics'] = False
    path.write_text(json.dumps(state))
    with engine(None, tmp_path/'display', prepared=path,
            research_guards=False, icount=None) as vm:
        command(vm, 'cont')
        deadline = time.monotonic()+5
        while True:
            panel = property(vm, 'display-status')
            if panel['known_pixels'] == 176 * 220: break
            assert time.monotonic() < deadline
            time.sleep(.005)
        command(vm, 'stop')
        complete = property(vm, 'display-cached')
        snapshot = vm.snapshot(time.monotonic()+5)
        assert complete['frames'] == 1
        assert len(bytes.fromhex(complete['pixels_hex'])) == 176 * 220 * 2
        assert complete['completed'] > 0
        assert snapshot['pl080']['raw_tc'] == 0x20
        assert snapshot['icount_shift'] is None
        assert snapshot['timing_profile'] == 'qemu-realtime-v1'


def test_native_timer_register_reads_during_concurrent_timer_callbacks(tmp_path):
    # TCG previews every architectural MMIO request before the actual access.
    # A short reload makes the I/O thread continuously update the same timer
    # while the CPU reads it, exposing unsynchronized phase copies.
    state = base_state(words(0xe5832000, 0xe5810000, 0xe591401c,
                            0xe2855001, 0xeafffffc))
    state.pop('instruction_limit')
    state['timebase'] = {'profile':'qemu-realtime-v1'}
    state['record_diagnostics'] = False
    state['registers'][:6] = [1, 0xf9010000, 0, 0xf9010004, 0, 0]
    state['timer_irq'] = {'profile':'tick1-upcounter-irq-assumption',
                          'frequency_hz':131072, 'max_irqs':8}
    state['device_registers'] = [
        {'address':address, 'width':4, 'initial_value':0, 'read_or_mask':0,
         'name':'fixture.irq-mask'}
        for address in (0xfb020104, 0xfb020204, 0xfb020118, 0xfb020218)]
    with engine(state, tmp_path/'timer-reads', research_guards=False, icount=None) as vm:
        before = property(vm, 'summary')
        command(vm, 'cont')
        until = time.monotonic() + 2
        last_ns = before['virtual_ns']
        while time.monotonic() < until:
            progress = property(vm, 'progress')
            assert progress['running'] is True
            assert progress['virtual_ns'] >= last_ns
            last_ns = progress['virtual_ns']
            time.sleep(.005)
        command(vm, 'stop')
        after = property(vm, 'progress')
        snapshot = vm.snapshot(time.monotonic()+5)
        assert snapshot['registers'][5] > 100
        assert after['timer_ticks'] > before['timer_ticks']
        assert after['host_profile']['timer-irq']['service_calls'] > 10


def test_native_mmio_guard_stop_releases_the_device_lock(tmp_path):
    state = sleeping_peripheral_state()
    state.pop('instruction_limit')
    state['timebase'] = {'profile':'qemu-realtime-v1'}
    state['record_diagnostics'] = False
    state['registers'][0] = 2  # Reject an unmodeled timer channel.
    with engine(state, tmp_path/'guard-stop', research_guards=False, icount=None) as vm:
        command(vm, 'cont')
        vm.wait_stop(time.monotonic()+5)
        assert property(vm, 'summary')['stop']['reason'] == 'timer_other_channel_unmodeled'
        assert command(vm, 'query-status')['running'] is False

"""Compiled differential checks against the retained W800 DB2010 import."""
from support.paths import RESOURCES
import re
import subprocess

import pytest

from support.component_test import ROOT, component_inputs

# The original research revision is absent from the imported fork's history.
BASELINE = '60db24c5ce984972a8d57a17ba61edafd08e09a2'


@pytest.mark.parametrize('model', ['simple', 'rtc-retained', 'dma', 'dsp-peer', 'memory-contracts'])
def test_migrated_map_matches_sealed_c(tmp_path, model):
    source = tmp_path / 'sealed.c'
    old = subprocess.check_output([
        'git', '-C', str(ROOT / 'qemu'), 'show',
        f'{BASELINE}:hw/arm/db2010-{model}.c',
    ]).decode()
    source.write_text(old)
    # Rename exported definitions only; calls to other components remain real.
    symbols = re.findall(r'^(?:const\s+)?(?:char\s*\*|bool|void|QDict\s*\*|u?int(?:32|64)_t|unsigned|int|DB2010\w+\s*\*?)\s*(\w+)\s*[\[(=]', old, re.M)
    obj = tmp_path / 'sealed.o'
    flags = component_inputs(model, *(['nor'] if model == 'memory-contracts' else []))
    subprocess.run([
        'cc', '-I' + str(ROOT / 'qemu/hw/arm'),
        *[f'-D{name}=sealed_{name}' for name in symbols],
        '-c', str(source), '-o', str(obj),
        *[arg for arg in flags if not arg.endswith('.c')],
    ], check=True)
    exe = tmp_path / 'parity'
    subprocess.run([
        'cc', '-Wall', '-Wextra', '-Werror', '-Wno-sign-compare',
        '-I' + str(ROOT / 'qemu/hw/arm'),
        str(RESOURCES / f'checks/map_{model.replace("-", "_")}.c'),
        str(obj), '-o', str(exe), *flags,
    ], check=True)
    subprocess.run([str(exe)], check=True)


def test_configured_register_views(tmp_path):
    exe = tmp_path / 'bank'
    subprocess.run([
        'cc', '-Wall', '-Wextra', '-Werror', '-Wno-sign-compare',
        '-I' + str(ROOT / 'qemu/hw/arm'),
        str(RESOURCES/'checks/map_bank.c'), '-o', str(exe),
        *component_inputs('register-bank', 'timer', 'memorystick'),
        str(ROOT / 'qemu/build/libqemuutil.a'),
    ], check=True)
    subprocess.run([str(exe)], check=True)

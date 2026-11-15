"""Drive ordinary browser keypad contacts and retain the firmware's LCD/logs.

This tool knows only contact names. It never recognizes service codes, changes
guest memory, or calls firmware handlers. Durations use the observed guest
clock so a slow host still gives the firmware time to scan press/release edges.
"""
import argparse
import asyncio
from contextlib import AsyncExitStack
import json
import math
from pathlib import Path
import time

from aiohttp import ClientSession, WSMsgType

from ..ui.keypad import BUTTONS
from ..ui.pixels import png_rgb565


class Bench:
    def __init__(self, output):
        self.output = output
        self.status = None
        self.panel = None
        self.pixels = b''
        self.uart = None
        self.input_enabled = False
        self.changed = asyncio.Event()
        self.inputs = []
        self.failure = None

    async def receive(self, socket):
        try:
            async for message in socket:
                if message.type == WSMsgType.BINARY:
                    size = int.from_bytes(message.data[:4], 'big')
                    header = json.loads(message.data[4:4 + size])
                    self.panel = header.get('panel')
                    self.pixels = message.data[4 + size:]
                elif message.type == WSMsgType.TEXT:
                    value = json.loads(message.data)
                    if value['type'] == 'status':
                        self.status = value['status']
                        self.input_enabled = value['input_enabled']
                    elif value['type'] == 'uart':
                        self.uart = value['uart']
                elif message.type == WSMsgType.ERROR:
                    raise RuntimeError(str(socket.exception()))
                self.changed.set()
        except Exception as error:
            self.failure = error
        finally:
            self.changed.set()

    async def ready(self):
        deadline = time.monotonic() + 15
        while self.status is None or self.panel is None:
            self.changed.clear()
            if self.failure:
                raise self.failure
            await asyncio.wait_for(self.changed.wait(), max(0.01, deadline - time.monotonic()))

    async def advance(self, seconds):
        if seconds == 0:
            return
        start = self.status.get('virtual_ns', 0)
        deadline = time.monotonic() + 60
        while True:
            self.changed.clear()
            if self.failure:
                raise self.failure
            if not self.status.get('running'):
                raise RuntimeError(f'Firmware stopped: {self.status}')
            if self.status.get('virtual_ns', 0) - start >= seconds * 1e9:
                return
            await asyncio.wait_for(self.changed.wait(), max(0.01, deadline - time.monotonic()))

    async def contact(self, socket, button, pressed):
        if not self.input_enabled:
            raise RuntimeError('Firmware input is disabled')
        message = {'type': 'keypad', 'button': button, 'pressed': pressed}
        await socket.send_json(message)
        self.inputs.append({**message, 'requested_monotonic': time.monotonic(),
                            'observed_virtual_ns': self.status.get('virtual_ns')})

    def capture(self, name):
        value = {'status': self.status, 'panel': self.panel, 'uart': self.uart}
        (self.output / f'{name}.json').write_text(json.dumps(value, indent=2) + '\n')
        if self.panel and self.panel.get('known_pixels') == 176 * 220 and len(self.pixels) == 176 * 220 * 2:
            (self.output / f'{name}.png').write_bytes(png_rgb565(self.pixels, 176, 220, 'big'))
        status = {key: self.status.get(key) for key in ('phase', 'running', 'pc', 'virtual_ns', 'stop', 'error')}
        print(json.dumps({'capture': name, 'status': status, 'panel': self.panel}), flush=True)


def duration(text):
    value = float(text)
    if not math.isfinite(value) or not 0 <= value <= 60:
        raise argparse.ArgumentTypeError('Use a finite duration between 0 and 60 guest seconds')
    return value


async def run(args):
    keys = args.keys.split(',') if args.keys else []
    prepare_keys = args.prepare_keys.split(',') if args.prepare_keys else []
    if any(key not in BUTTONS for key in keys + prepare_keys):
        raise ValueError(f'Unknown contact; choose from {sorted(BUTTONS)}')
    args.output.mkdir(parents=True, exist_ok=False)
    bench = Bench(args.output)
    async with AsyncExitStack() as resources:
        client = await resources.enter_async_context(ClientSession())
        async with client.ws_connect(args.url.rstrip('/') + '/ws', origin=args.url.rstrip('/')) as socket:
            receiver = asyncio.create_task(bench.receive(socket))
            try:
                page = None
                if args.on_screen:
                    from playwright.async_api import async_playwright
                    playwright = await resources.enter_async_context(async_playwright())
                    browser = await playwright.chromium.launch(headless=True, args=['--disable-webgl'])
                    resources.push_async_callback(browser.close)
                    page = await browser.new_page(viewport={'width': 1280, 'height': 1600})
                    await page.goto(args.url)
                    await page.wait_for_function("""() => !document.querySelector('#lcd').hidden ||
                        !document.querySelector('#view-toggle').disabled""")
                    toggle = page.locator('#view-toggle')
                    if await toggle.is_visible() and await toggle.is_enabled():
                        await toggle.click()
                    await page.locator('#handset').scroll_into_view_if_needed()
                await bench.ready()
                if args.resume_control:
                    (args.resume_control / 'request.json').write_text(json.dumps({'resume': True}))
                    deadline = time.monotonic() + 15
                    while not bench.status.get('running'):
                        bench.changed.clear()
                        await asyncio.wait_for(bench.changed.wait(), max(0.01, deadline - time.monotonic()))
                async def press(key):
                    if page:
                        bench.inputs.append({'button': key, 'source': 'on-screen pointer',
                            'requested_monotonic': time.monotonic(),
                            'observed_virtual_ns': bench.status.get('virtual_ns')})
                        box = await page.locator(f'button[data-key="{key}"]').bounding_box()
                        if not box:
                            raise RuntimeError(f'On-screen contact {key} is unavailable')
                        await page.mouse.move(box['x'] + box['width'] / 2, box['y'] + box['height'] / 2)
                        await page.mouse.down()
                        await asyncio.sleep(0.25)
                        await page.mouse.up()
                        await asyncio.sleep(0.25)
                    else:
                        await bench.contact(socket, key, True)
                        await bench.advance(args.hold_guest)
                        await bench.contact(socket, key, False)
                        await bench.advance(args.gap_guest)
                for key in prepare_keys:
                    await press(key)
                    await bench.advance(1)
                await bench.advance(args.wait_guest)
                bench.capture('00-before')
                for index, key in enumerate(keys, 1):
                    await press(key)
                    if page and args.gap_guest != 0.25:
                        await bench.advance(args.gap_guest)
                    bench.capture(f'{index:02d}-{key}')
                await bench.advance(args.after_guest)
                bench.capture('final')
            finally:
                (args.output / 'requested-inputs.json').write_text(json.dumps(bench.inputs, indent=2) + '\n')
                bench.capture('last')
                receiver.cancel()
                await asyncio.gather(receiver, return_exceptions=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--url', default='http://127.0.0.1:8765')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--keys', default='', help='Comma-separated ordinary contact names')
    parser.add_argument('--prepare-keys', default='', help='Contacts to apply with a one-second settle before the capture')
    parser.add_argument('--resume-control', type=Path, help='Resume a paused diagnostic runner after the on-screen keypad is ready')
    parser.add_argument('--on-screen', action='store_true', help='Click the actual on-screen keypad at human pace')
    parser.add_argument('--wait-guest', type=duration, default=0)
    parser.add_argument('--hold-guest', type=duration, default=0.15)
    parser.add_argument('--gap-guest', type=duration, default=0.25)
    parser.add_argument('--after-guest', type=duration, default=1)
    asyncio.run(run(parser.parse_args()))


if __name__ == '__main__':
    main()

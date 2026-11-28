#!/usr/bin/env python3
"""Build the DB2010 engine as an Android PIE executable in the APK's lib dir.

The official NDK headers/libraries work with native LLVM on Linux ARM64, where
Google's Linux x86-64 compiler cannot run. Desktop Linux x86-64 can use it too.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT/'src'))
from db2010.development.source import apply_patches
SOURCES = {
    'libiconv-1.17': 'https://ftp.gnu.org/pub/gnu/libiconv/libiconv-1.17.tar.gz',
    'libffi-3.4.6': 'https://github.com/libffi/libffi/releases/download/v3.4.6/libffi-3.4.6.tar.gz',
    'pcre2-10.46': 'https://github.com/PCRE2Project/pcre2/releases/download/pcre2-10.46/pcre2-10.46.tar.bz2',
    'glib-2.84.4': 'https://download.gnome.org/sources/glib/2.84/glib-2.84.4.tar.xz',
}
MIRRORS = {
    'glib-2.84.4': ('https://deb.debian.org/debian/pool/main/g/glib2.0/glib2.0_2.84.4.orig.tar.xz',),
}
HASHES = {
    'libiconv-1.17': '8f74213b56238c85a50a5329f77e06198771e70dd9a739779f4c02f65d971313',
    'libffi-3.4.6': 'b0dea9df23c863a7a50e825440f3ebffabd65df1497108e5d437747843895a4e',
    'pcre2-10.46': '15fbc5aba6beee0b17aecb04602ae39432393aba1ebd8e39b7cabf7db883299f',
    'glib-2.84.4': '8a9ea10943c36fc117e253f80c91e477b673525ae45762942858aef57631bb90',
}
TSF_URL = 'https://raw.githubusercontent.com/schellingb/TinySoundFont/a3621a19c6504885cd9583e6e26c35bc954fc8ec/tsf.h'
TSF_SHA256 = '70d55963c98f60ebb81518eaa1f25d46888d5180eb5f5289fd6b74ffc177d197'


def run(argv, cwd, env):
    print('+', ' '.join(map(str, argv)), flush=True)
    subprocess.run(list(map(str, argv)), cwd=cwd, env=env, check=True)


def setup_tools(ndk, build, clang):
    prebuilt = ndk/'toolchains/llvm/prebuilt/linux-x86_64'
    sysroot = prebuilt/'sysroot'
    builtins = next(prebuilt.glob('lib/clang/*/lib/linux/libclang_rt.builtins-aarch64-android.a'))
    resource = builtins.parents[2]
    include = Path(subprocess.check_output([clang, '-print-resource-dir'], text=True).strip())/'include'
    tool = build/'tools'; tool.mkdir(parents=True, exist_ok=True)
    llvm = Path(clang).resolve().parent
    for name, compiler in [('cc', clang), ('cxx', str(llvm/'clang++'))]:
        wrapper = tool/name
        wrapper.write_text('#!'+shutil.which('python3')+'\n'+
            'import os, sys\n'+
            'base='+repr([compiler, '--target=aarch64-linux-android24', '--sysroot='+str(sysroot),
                # The NDK resource directory supplies Android runtime archives.
                # Use just the host compiler's builtin headers: two Clang
                # stdint.h headers share a guard and break include_next.
                '-resource-dir='+str(resource), '-nobuiltininc', '-isystem', str(include), '-fPIC'])+'\n'+
            'args=[a for a in sys.argv[1:] if not a.startswith("--target=aarch64-linux-android")]\n'+
            'if not any(arg in args for arg in ["-c","-E","-S","--version","-dumpmachine"]):\n'+
            '    base+='+repr(['-fuse-ld='+str(llvm/'ld.lld'), '-rtlib=compiler-rt', '--unwindlib=none',
                             '-L'+str(builtins.parent/'aarch64'),
                             '-Wl,-z,max-page-size=16384'])+'\n'+
            'os.execv(base[0],base+args)\n')
        wrapper.chmod(0o755)
    for name, target in [('ar','llvm-ar'), ('strip','llvm-strip'), ('ranlib','llvm-ranlib'), ('nm','llvm-nm')]:
        path=tool/name
        executable = llvm/target if (llvm/target).is_file() else Path(shutil.which(name))
        if path.is_symlink() and not path.exists(): path.unlink()
        if not path.exists(): path.symlink_to(executable)
    prefix=build/'prefix'; (prefix/'lib/pkgconfig').mkdir(parents=True, exist_ok=True)
    # Android clock/pthread APIs are in libc; there is no separate librt.
    (prefix/'lib/librt.a').write_bytes(b'!<arch>\n')
    # zlib is part of the NDK's audited Android sysroot.
    (prefix/'lib/pkgconfig/zlib.pc').write_text(
        f'prefix={sysroot}/usr\nlibdir={sysroot}/usr/lib/aarch64-linux-android\n'
        'Name: zlib\nDescription: Android NDK zlib\nVersion: 1.3\n'
        f'Libs: {sysroot}/usr/lib/aarch64-linux-android/libz.a\nCflags: -I{sysroot}/usr/include\n')
    pkg=tool/'pkg-config'
    pkg.write_text('#!/bin/sh\nexport PKG_CONFIG_LIBDIR='+str(prefix/'lib/pkgconfig')+'\nexec /usr/bin/pkg-config "$@"\n')
    pkg.chmod(0o755)
    cross=build/'android.ini'
    cross.write_text('[binaries]\n'+''.join(f"{key} = '{value}'\n" for key,value in [
        ('c',tool/'cc'),('cpp',tool/'cxx'),('ar',tool/'ar'),('strip',tool/'strip'),('pkg-config',pkg)])+
        "[host_machine]\nsystem = 'android'\ncpu_family = 'aarch64'\ncpu = 'aarch64'\nendian = 'little'\n"
        "[properties]\nneeds_exe_wrapper = true\n"+
        f"[built-in options]\nc_args = ['-I{prefix}/include']\nc_link_args = ['-L{prefix}/lib']\n")
    env=os.environ.copy()
    temporary=build/'tmp'; temporary.mkdir(exist_ok=True)
    env.update(CC=str(tool/'cc'), CXX=str(tool/'cxx'), AR=str(tool/'ar'), RANLIB=str(tool/'ranlib'),
        STRIP=str(tool/'strip'), PKG_CONFIG=str(pkg), PKG_CONFIG_LIBDIR=str(prefix/'lib/pkgconfig'),
        CFLAGS=f'-O2 -fPIC -I{prefix}/include', LDFLAGS=f'-L{prefix}/lib',
        ANDROID_NDK_HOME=str(ndk), ANDROID_NDK_ROOT=str(ndk),
        FEATUREPHONE_ANDROID_HOST_CLANG=str(tool/'cc'), TMPDIR=str(temporary))
    return tool, prefix, cross, env


def download(name, url, sha256, build):
    downloads=build/'downloads'; downloads.mkdir(exist_ok=True)
    file=downloads/url.rsplit('/',1)[1]
    if not file.is_file():
        partial = file.with_name(file.name + '.part')
        for candidate in (url, *MIRRORS.get(name, ())):
            print('Downloading', candidate, flush=True)
            try:
                with urllib.request.urlopen(candidate, timeout=12) as response, partial.open('wb') as output:
                    shutil.copyfileobj(response, output)
                if hashlib.sha256(partial.read_bytes()).hexdigest() != sha256:
                    raise ValueError('checksum differs')
                partial.replace(file)
                break
            except (urllib.error.URLError, TimeoutError, ValueError) as error:
                print(f'Download failed: {error}', flush=True)
            finally:
                partial.unlink(missing_ok=True)
        else:
            raise RuntimeError('Could not download a verified Android dependency: ' + name)
    if hashlib.sha256(file.read_bytes()).hexdigest()!=sha256:
        raise ValueError('Android dependency identity differs: '+name)
    return file, {'url':url,'sha256':sha256,'bytes':file.stat().st_size}


def source(name, url, build):
    file, identity = download(name, url, HASHES[name], build)
    directory=build/'sources'/name
    if not directory.is_dir():
        directory.parent.mkdir(exist_ok=True)
        with tarfile.open(file) as archive: archive.extractall(directory.parent,filter='data')
    return directory, identity


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--ndk',type=Path,required=True)
    parser.add_argument('--build',type=Path,default=ROOT/'build/android/qemu')
    parser.add_argument('--clang',default=os.environ.get('FEATUREPHONE_ANDROID_CLANG') or shutil.which('clang-19') or shutil.which('clang'))
    parser.add_argument('--jobs',type=int,default=2)
    parser.add_argument('--tools-only',action='store_true')
    args=parser.parse_args()
    args.build=args.build.resolve(); args.build.mkdir(parents=True,exist_ok=True)
    tool,prefix,cross,env=setup_tools(args.ndk.resolve(),args.build,args.clang)
    if args.tools_only:
        print(json.dumps({'tools':str(tool),'prefix':str(prefix)})); return
    lock_path = ROOT/'dependencies/qemu/source-lock.json'
    lock = json.loads(lock_path.read_text())
    revision = apply_patches(ROOT, lock)
    provenance={'created_unix':time.time(),'ndk':str(args.ndk.resolve()),'sources':{},'engine_source':
        revision, 'engine_patches':lock.get('patches', []),
        'source_lock_sha256':hashlib.sha256(lock_path.read_bytes()).hexdigest()}
    tsf_header, provenance['sources']['TinySoundFont'] = download('TinySoundFont', TSF_URL, TSF_SHA256, args.build)
    for name,url in SOURCES.items():
        directory,identity=source(name,url,args.build); provenance['sources'][name]=identity
        marker=args.build/(name+'.built')
        if marker.is_file(): continue
        if name.startswith('libiconv') or name.startswith('libffi'):
            run([directory/'configure','--host=aarch64-linux-android', '--prefix='+str(prefix),
                 '--disable-shared','--enable-static','--disable-docs'],directory,env)
            run(['make','-j'+str(args.jobs)],directory,env); run(['make','install'],directory,env)
        elif name.startswith('pcre2'):
            target=args.build/'pcre2-build'
            run(['cmake','-S',directory,'-B',target,'-DCMAKE_SYSTEM_NAME=Linux',
                '-DCMAKE_C_COMPILER='+str(tool/'cc'), '-DCMAKE_INSTALL_PREFIX='+str(prefix),
                '-DBUILD_SHARED_LIBS=OFF','-DPCRE2_BUILD_TESTS=OFF','-DPCRE2_BUILD_PCRE2GREP=OFF',
                '-DPCRE2_BUILD_PCRE2_16=OFF','-DPCRE2_BUILD_PCRE2_32=OFF','-DPCRE2_SUPPORT_JIT=ON'],directory,env)
            run(['cmake','--build',target,'-j',str(args.jobs)],directory,env)
            run(['cmake','--install',target],directory,env)
        else:
            target=args.build/'glib-build'
            # Use the active build environment; the QEMU build directory can
            # contain an incomplete virtual environment from an earlier job.
            meson=shutil.which('meson') or ROOT/'.venv/bin/meson'
            if not Path(meson).is_file():
                raise RuntimeError('Meson is required to build GLib. Install meson==1.11.1 in the build Python environment.')
            text=(directory/'meson.options' if (directory/'meson.options').is_file() else directory/'meson_options.txt').read_text()
            types=dict(re.findall(r"option\(\s*'([^']+)'\s*,\s*type\s*:\s*'([^']+)'",text))
            disabled=[]
            for name in ['tests','installed_tests','nls','libmount','libelf','selinux','introspection','documentation','man-pages','gtk_doc','sysprof']:
                if types.get(name) in ('boolean','feature'):
                    disabled.append('-D'+name+'='+('false' if types[name]=='boolean' else 'disabled'))
            run([meson,'setup',target,directory,'--cross-file',cross,'--prefix',prefix,'--libdir','lib',
                 '--buildtype','release','-Ddefault_library=static',*disabled],directory,env)
            run(['ninja','-C',target,'-j',str(args.jobs)],directory,env)
            run([meson,'install','-C',target],directory,env)
        marker.write_text(json.dumps(identity))
    target=args.build/'engine-build'; target.mkdir(exist_ok=True)
    support=ROOT/'frontends/android/build-support'
    compat=args.build/'android-compat.o'
    run([tool/'cc', '-O2', '-c', support/'android_compat.c', '-o', compat],args.build,env)
    options=[flag for flag in lock['configure'] if flag not in ['--enable-plugins']]
    options += ['--disable-plugins','--disable-gio','--disable-pixman','--enable-fdt',
                '--disable-kvm','--disable-vhost-kernel','--disable-vhost-net','--disable-vhost-user',
                '--disable-vhost-crypto','--disable-vhost-vdpa','--disable-vhost-user-blk-server',
                '--cross-prefix='+str(tool)+'/', '--cpu=aarch64', '--enable-pie',
                '--disable-debug-info','--cc='+str(tool/'cc'),'--cxx='+str(tool/'cxx'),
                '--host-cc=cc','--extra-cflags=-I'+str(prefix/'include')+' -include '+str(support/'android_compat.h')+
                ' -DCONFIG_DB2010_SOUNDFONT -isystem '+str(tsf_header.parent),
                '--extra-ldflags=-L'+str(prefix/'lib')+' -liconv '+str(compat)]
    configuration={'options':options,'compiler_sha256':hashlib.sha256((tool/'cc').read_bytes()).hexdigest()}
    config_file=target/'android-config.json'
    if not (target/'build.ninja').is_file() or not config_file.is_file() or json.loads(config_file.read_text())!=configuration:
        python=ROOT/'.venv/bin/python'
        run([ROOT/'qemu/configure','--python='+str(python if python.is_file() else sys.executable),*options],target,env)
        config_file.write_text(json.dumps(configuration,indent=2))
    run(['ninja','-C',target,'-j',str(args.jobs),'qemu-system-arm'],target,env)
    output=ROOT/'frontends/android/app/build/generated/e830JniLibs/arm64-v8a/libdb2010_engine.so'
    output.parent.mkdir(parents=True,exist_ok=True)
    shutil.copyfile(target/'qemu-system-arm',output)
    run([tool/'strip','--strip-unneeded',output],target,env)
    output.chmod(0o755)
    provenance['engine_sha256']=hashlib.sha256(output.read_bytes()).hexdigest()
    provenance['output']=str(output); provenance['bytes']=output.stat().st_size
    (args.build/'provenance.json').write_text(json.dumps(provenance,indent=2)+'\n')
    print('Packaged',output,flush=True)


if __name__=='__main__': main()

import subprocess
import sys

def run(cmd):
    print(f"Running: {' '.join(cmd)}")
    res = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, shell=True)
    print(res.stdout)

run(["winget", "search", "sdl2"])
run(["winget", "list", "sdl2"])

import os

search_dirs = [
    r"C:\Program Files",
    r"C:\Program Files (x86)",
    r"C:\vcpkg",
    r"C:\tools",
    r"C:\msys64",
    r"C:\Users\alber\AppData\Local"
]

found = []
for sd in search_dirs:
    if os.path.exists(sd):
        print(f"Searching in {sd}...")
        for root, dirs, files in os.walk(sd):
            # Limit depth of search to avoid running forever
            depth = root[len(sd):].count(os.sep)
            if depth > 4:
                # Skip deep directories
                dirs.clear()
                continue
            for file in files:
                if "sdl2" in file.lower() and (file.endswith(".cmake") or file.endswith(".lib") or file.endswith(".dll")):
                    path = os.path.join(root, file)
                    print(f"Found: {path}")
                    found.append(path)
                    
print("Search finished.")

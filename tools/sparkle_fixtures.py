# Synthetic bundle metadata; independent of the application release version.
INSTALLED_VERSION = "1.2.3"
UPDATE_VERSION = "1.2.4"
SUPPORTED_PROTOCOLS = [6, 7]
REPLACEMENT_STARTED = f"started {UPDATE_VERSION} "
ROUTINE_CHANNEL = f"major-{INSTALLED_VERSION.split('.')[0]}-protocols-" + "-".join(map(str, SUPPORTED_PROTOCOLS))
MAJOR_UPDATE_VERSION = f"{int(INSTALLED_VERSION.split('.')[0]) + 1}.0.0"

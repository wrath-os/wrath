# Where a user's home is. Modules load it with `load("lib/home.star", "home_of")`.

def home_of(name, user):
    if "home" in user:
        return user["home"]
    if user["system"]:
        return "/var/empty"
    return "/home/" + name

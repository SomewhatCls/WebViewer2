# WebViewer2
### Project Overview
This is a simple, multi-tabbed browser, written with Rust in the Tauri framework. It utilizes Windows WebView2 for displaying web content. Because of this, it is much more RAM-Efficient than a conventional browser.
### Features
This Browser offers an optional browsing history, as well as the option to serve address bar suggestions from said history. \
chrome web extensions are supported. For this, about:extensions serves as a hub. Adding extensions works by either adding them directly through about:extensions or by visiting the chrome webstore and clicking the button in the navigation bar. \
Important: Do not press the chrome webstores install button, as this will fail.
Additionally, you can toggle session persistence, which saves your open tabs and reopens them on reboot.\
Finally, the browser has cli support to enable development work, as well as the WebView2 development console.\
the syntax for the cli is as follows: "WebViewer2.exe example.com". \
To open the development console, use the shortcut "ctrl+shift+i".
### Design
This browser tries to follow the [Fluent 2](https://fluent2.microsoft.design/) design language by Microsoft.
To this end, it applies the Mica background effect as long as you are on Windows 11 with transparency turned on. An Example image is attached below.

<img width="2092" height="1524" alt="image" src="https://github.com/user-attachments/assets/c8663b0b-35d7-4c05-872a-684ce91eefb9" />

import { mount } from 'svelte';
import '@material/web/button/text-button.js';
import '@material/web/button/filled-button.js';
import '@material/web/divider/divider.js';
import '@material/web/iconbutton/icon-button.js';
import '@material/web/labs/gb/components/badge/md-gb-badge.js';
import '@material/web/menu/menu.js';
import '@material/web/menu/menu-item.js';
import '@material/web/progress/linear-progress.js';
import App from './App.svelte';
import './style.css';

mount(App, { target: document.getElementById('app') });

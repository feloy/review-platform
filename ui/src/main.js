import { mount } from 'svelte';
import '@material/web/divider/divider.js';
import '@material/web/progress/linear-progress.js';
import App from './App.svelte';
import './style.css';

mount(App, { target: document.getElementById('app') });
